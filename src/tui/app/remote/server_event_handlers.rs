use super::*;

pub(super) fn handle_tool_done(
    app: &mut App,
    remote: &mut impl RemoteEventState,
    id: String,
    name: String,
    output: String,
    error: Option<String>,
) -> bool {
    // Check if this tool has a pending entry that already rendered the tool header.
    // If so, use it to render the full tool result now instead of creating a duplicate.
    if let Some(pending_tc) = app.pending_tool_entry.take() {
        if pending_tc.id == id {
            // Render the complete tool result using the pending entry
            let display_output = if error.is_some()
                && !output.starts_with("Error:")
                && !output.starts_with("error:")
                && !output.starts_with("Failed:")
            {
                format!("Error: {}", output)
            } else {
                output.clone()
            };
            let is_batch = pending_tc.name == "batch";
            app.observe_tool_result(&pending_tc, &output, error.is_some(), None);
            app.push_display_message(DisplayMessage {
                role: "tool".to_string(),
                content: display_output,
                tool_calls: vec![],
                duration_secs: None,
                title: None,
                tool_data: Some(pending_tc),
            });
            if is_batch {
                app.batch_progress = None;
            }
            app.streaming_tool_calls.clear();
            app.status = ProcessingStatus::Streaming;
            return true;
        } else {
            // ID mismatch — put it back and fall through to normal path
            app.pending_tool_entry = Some(pending_tc);
        }
    }

    // Normal path: no pending entry, render tool result directly
    let display_output = remote.handle_tool_done(&id, &name, &output);
    let display_output = if error.is_some()
        && !display_output.starts_with("Error:")
        && !display_output.starts_with("error:")
        && !display_output.starts_with("Failed:")
    {
        format!("Error: {}", display_output)
    } else {
        display_output
    };
    let existing_tool_call = app
        .streaming_tool_calls
        .iter()
        .find(|tc| tc.id == id)
        .cloned();
    let tool_call = existing_tool_call.unwrap_or_else(|| ToolCall {
        id: id.clone(),
        name: name.clone(),
        input: serde_json::Value::Null,
        intent: None,
    });
    app.commit_pending_streaming_assistant_message();
    crate::tui::mermaid::clear_streaming_preview_diagram();
    let is_batch = tool_call.name == "batch";
    app.observe_tool_result(&tool_call, &output, error.is_some(), None);
    app.push_display_message(DisplayMessage {
        role: "tool".to_string(),
        content: display_output,
        tool_calls: vec![],
        duration_secs: None,
        title: None,
        tool_data: Some(tool_call),
    });
    if is_batch {
        app.batch_progress = None;
    }
    app.streaming_tool_calls.clear();
    app.status = ProcessingStatus::Streaming;
    true
}

pub(super) fn handle_generated_image(
    app: &mut App,
    id: String,
    path: String,
    metadata_path: Option<String>,
    output_format: String,
    revised_prompt: Option<String>,
) -> bool {
    app.pause_streaming_tps(false);
    app.commit_pending_streaming_assistant_message();
    let input = crate::message::generated_image_tool_input(
        &path,
        metadata_path.as_deref(),
        &output_format,
        revised_prompt.as_deref(),
    );
    let tool_call = ToolCall {
        id: id.clone(),
        name: crate::message::GENERATED_IMAGE_TOOL_NAME.to_string(),
        input,
        intent: Some("OpenAI native image generation".to_string()),
    };
    let summary = crate::message::generated_image_summary(
        &path,
        metadata_path.as_deref(),
        &output_format,
        revised_prompt.as_deref(),
    );
    app.push_display_message(DisplayMessage {
        role: "tool".to_string(),
        content: summary,
        tool_calls: vec![],
        duration_secs: None,
        title: Some("Generated image".to_string()),
        tool_data: Some(tool_call),
    });
    app.status = ProcessingStatus::Streaming;
    true
}
