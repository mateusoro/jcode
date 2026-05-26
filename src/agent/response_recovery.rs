use super::*;

impl Agent {
    fn parse_text_wrapped_tool_call(
        text: &str,
    ) -> Option<(String, String, serde_json::Value, String)> {
        // Try Claude format first: "to=functions.<tool_name>"
        if let Some(result) = Self::parse_claude_format(text) {
            return Some(result);
        }
        // Try MiniMax XML-like format:
        // <minimax:tool_call><invoke name="bash"><parameter ...>...</parameter></invoke></minimax:tool_call>
        if let Some(result) = Self::parse_minimax_xml_format(text) {
            return Some(result);
        }
        // Try MiniMax function format: "minimax:tool_call(<tool_name>, {json})"
        Self::parse_minimax_format(text)
    }

    fn parse_claude_format(text: &str) -> Option<(String, String, serde_json::Value, String)> {
        let marker = "to=functions.";
        let marker_idx = text.find(marker)?;
        let after_marker = &text[marker_idx + marker.len()..];

        let mut tool_name_end = 0usize;
        for (idx, ch) in after_marker.char_indices() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                tool_name_end = idx + ch.len_utf8();
            } else {
                break;
            }
        }
        if tool_name_end == 0 {
            return None;
        }

        let tool_name = after_marker[..tool_name_end].to_string();
        let remaining = &after_marker[tool_name_end..];
        Self::parse_json_arguments(&tool_name, text, marker_idx, remaining)
    }

    fn parse_minimax_format(text: &str) -> Option<(String, String, serde_json::Value, String)> {
        // Format: "minimax:tool_call(<tool_name>, {json...})"
        let marker = "minimax:tool_call(";
        let marker_idx = text.find(marker)?;
        let after_marker = &text[marker_idx + marker.len()..];

        // Extract tool name (until first comma)
        let comma_idx = after_marker.find(',')?;
        let tool_name = after_marker[..comma_idx].trim().to_string();

        // Extract JSON arguments (everything after the comma until closing paren)
        let args_start = comma_idx + 1;
        let args_text = &after_marker[args_start..];

        // Find matching closing paren for the outer tool_call(
        let mut paren_depth = 1;
        let mut json_end = 0;
        for (idx, ch) in args_text.char_indices() {
            match ch {
                '(' | '{' | '[' => paren_depth += 1,
                ')' | '}' | ']' => {
                    paren_depth -= 1;
                    if paren_depth == 0 {
                        json_end = idx;
                        break;
                    }
                }
                _ => {}
            }
        }

        if json_end == 0 {
            return None;
        }

        let json_str = args_text[..json_end].trim();
        let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;

        let prefix = text[..marker_idx].trim_end().to_string();
        let suffix = args_text[json_end + 1..].trim().to_string();

        Some((prefix, tool_name, parsed, suffix))
    }

    fn parse_minimax_xml_format(text: &str) -> Option<(String, String, serde_json::Value, String)> {
        let start_marker = "<minimax:tool_call>";
        let end_marker = "</minimax:tool_call>";
        let marker_idx = text.find(start_marker)?;
        let after_start_idx = marker_idx + start_marker.len();
        let after_start = &text[after_start_idx..];
        let relative_end_idx = after_start.find(end_marker)?;
        let inner = &after_start[..relative_end_idx];
        let suffix = after_start[relative_end_idx + end_marker.len()..]
            .trim()
            .to_string();

        let invoke_marker = "<invoke";
        let invoke_idx = inner.find(invoke_marker)?;
        let invoke_after = &inner[invoke_idx + invoke_marker.len()..];
        let invoke_tag_end = invoke_after.find('>')?;
        let invoke_attrs = &invoke_after[..invoke_tag_end];
        let tool_name = Self::extract_xml_attr(invoke_attrs, "name")?;
        if tool_name.trim().is_empty() {
            return None;
        }

        let invoke_body = &invoke_after[invoke_tag_end + 1..];
        let invoke_body = invoke_body.split("</invoke>").next().unwrap_or(invoke_body);

        let mut args = serde_json::Map::new();
        let mut rest = invoke_body;
        let parameter_marker = "<parameter";
        while let Some(param_idx) = rest.find(parameter_marker) {
            let after_param = &rest[param_idx + parameter_marker.len()..];
            let Some(param_tag_end) = after_param.find('>') else {
                break;
            };
            let param_attrs = &after_param[..param_tag_end];
            let Some(param_name) = Self::extract_xml_attr(param_attrs, "name") else {
                rest = &after_param[param_tag_end + 1..];
                continue;
            };
            let value_start = param_tag_end + 1;
            let after_value_start = &after_param[value_start..];
            let Some(value_end) = after_value_start.find("</parameter>") else {
                break;
            };
            let raw_value = &after_value_start[..value_end];
            let value = Self::decode_xml_entities(raw_value.trim());
            args.insert(param_name, serde_json::Value::String(value));
            rest = &after_value_start[value_end + "</parameter>".len()..];
        }

        if args.is_empty() {
            return None;
        }

        let prefix = text[..marker_idx].trim_end().to_string();
        Some((prefix, tool_name, serde_json::Value::Object(args), suffix))
    }

    fn extract_xml_attr(attrs: &str, name: &str) -> Option<String> {
        let needle = format!("{name}=");
        let start = attrs.find(&needle)? + needle.len();
        let rest = attrs[start..].trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let value = &rest[quote.len_utf8()..];
        let end = value.find(quote)?;
        Some(Self::decode_xml_entities(&value[..end]))
    }

    fn decode_xml_entities(value: &str) -> String {
        value
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    fn parse_json_arguments(
        tool_name: &str,
        text: &str,
        marker_idx: usize,
        remaining: &str,
    ) -> Option<(String, String, serde_json::Value, String)> {
        let mut fallback: Option<(String, String, serde_json::Value, String)> = None;

        for (brace_idx, ch) in remaining.char_indices() {
            if ch != '{' {
                continue;
            }
            let slice = &remaining[brace_idx..];
            let mut stream =
                serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
            let parsed = match stream.next() {
                Some(Ok(value)) => value,
                Some(Err(_)) | None => continue,
            };
            let consumed = stream.byte_offset();
            if !parsed.is_object() {
                continue;
            }

            let prefix = text[..marker_idx].trim_end().to_string();
            let suffix = remaining[brace_idx + consumed..].trim().to_string();
            if suffix.is_empty() {
                return Some((prefix, tool_name.to_string(), parsed, suffix));
            }
            if fallback.is_none() {
                fallback = Some((prefix, tool_name.to_string(), parsed, suffix));
            }
        }

        fallback
    }

    pub(super) fn recover_text_wrapped_tool_call(
        &self,
        text_content: &mut String,
        tool_calls: &mut Vec<ToolCall>,
    ) -> bool {
        if !tool_calls.is_empty() || text_content.trim().is_empty() {
            return false;
        }

        let Some((prefix, tool_name, arguments, suffix)) =
            Self::parse_text_wrapped_tool_call(text_content)
        else {
            return false;
        };

        let mut sanitized = String::new();
        if !prefix.is_empty() {
            sanitized.push_str(&prefix);
        }
        if !suffix.is_empty() {
            if !sanitized.is_empty() {
                sanitized.push('\n');
            }
            sanitized.push_str(&suffix);
        }
        *text_content = sanitized;

        let call_id = format!("fallback_text_call_{}", id::new_id("call"));
        let recovered_total = RECOVERED_TEXT_WRAPPED_TOOL_CALLS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        logging::warn(&format!(
            "[agent] Recovered text-wrapped tool call for '{}' ({}, total={})",
            tool_name, call_id, recovered_total
        ));
        let intent = ToolCall::intent_from_input(&arguments);
        tool_calls.push(ToolCall {
            id: call_id,
            name: tool_name,
            input: arguments,
            intent,
        });

        true
    }

    pub(super) fn should_continue_after_stop_reason(stop_reason: &str) -> bool {
        let reason = stop_reason.trim().to_ascii_lowercase();
        if reason.is_empty() {
            return false;
        }

        if matches!(reason.as_str(), "stop" | "end_turn" | "tool_use") {
            return false;
        }

        reason.contains("incomplete")
            || reason.contains("max_output_tokens")
            || reason.contains("max_tokens")
            || reason.contains("length")
            || reason.contains("trunc")
            || reason.contains("commentary")
    }
    fn continuation_prompt_for_stop_reason(stop_reason: &str) -> String {
        format!(
            "[System reminder: your previous response ended before completion (stop_reason: {}). Continue exactly where you left off, do not repeat completed content, and if the next step is a tool call, emit the tool call now.]",
            stop_reason.trim()
        )
    }

    pub(crate) fn maybe_continue_incomplete_response(
        &mut self,
        stop_reason: Option<&str>,
        attempts: &mut u32,
    ) -> Result<bool> {
        let Some(stop_reason) = stop_reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
        else {
            return Ok(false);
        };

        if !Self::should_continue_after_stop_reason(stop_reason) {
            return Ok(false);
        }

        if *attempts >= Self::MAX_INCOMPLETE_CONTINUATION_ATTEMPTS {
            logging::warn(&format!(
                "Response ended with stop_reason='{}' after {} continuation attempts; returning partial output",
                stop_reason, attempts
            ));
            return Ok(false);
        }

        *attempts += 1;
        logging::warn(&format!(
            "Response ended with stop_reason='{}'; requesting continuation (attempt {}/{})",
            stop_reason,
            attempts,
            Self::MAX_INCOMPLETE_CONTINUATION_ATTEMPTS
        ));

        self.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: Self::continuation_prompt_for_stop_reason(stop_reason),
                cache_control: None,
            }],
        );
        self.session.save()?;
        Ok(true)
    }

    pub(super) fn filter_truncated_tool_calls(
        &mut self,
        stop_reason: Option<&str>,
        tool_calls: &mut Vec<ToolCall>,
        assistant_message_id: Option<&String>,
    ) {
        let stop_reason = stop_reason.unwrap_or("");
        if !Self::should_continue_after_stop_reason(stop_reason) {
            return;
        }

        let before = tool_calls.len();
        tool_calls.retain(|tc| !tc.input.is_null());
        let discarded = before - tool_calls.len();
        if discarded > 0 && tool_calls.is_empty() {
            logging::warn(&format!(
                "Discarded {} tool call(s) with null input (truncated by {}); requesting continuation",
                discarded,
                if stop_reason.is_empty() {
                    "unknown"
                } else {
                    stop_reason
                }
            ));
            if let Some(msg_id) = assistant_message_id {
                self.session.remove_tool_use_blocks(msg_id);
                self.persist_session_best_effort("truncated tool-call repair");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimax_xml_wrapped_tool_call() {
        let text = r#"Before
<minimax:tool_call>
<invoke name="bash">
<parameter name="command">printf JCODE_TOOL_OK</parameter>
<parameter name="timeout">120000</parameter>
</invoke>
</minimax:tool_call>
After"#;

        let (prefix, tool_name, arguments, suffix) =
            Agent::parse_text_wrapped_tool_call(text).expect("MiniMax XML tool call should parse");

        assert_eq!(prefix, "Before");
        assert_eq!(tool_name, "bash");
        assert_eq!(arguments["command"], "printf JCODE_TOOL_OK");
        assert_eq!(arguments["timeout"], "120000");
        assert_eq!(suffix, "After");
    }

    #[test]
    fn parses_minimax_function_wrapped_tool_call() {
        let text = r#"minimax:tool_call(bash, {"command":"printf JCODE_TOOL_OK"})"#;
        let (_prefix, tool_name, arguments, suffix) = Agent::parse_text_wrapped_tool_call(text)
            .expect("MiniMax function tool call should parse");

        assert_eq!(tool_name, "bash");
        assert_eq!(arguments["command"], "printf JCODE_TOOL_OK");
        assert!(suffix.is_empty());
    }
}
