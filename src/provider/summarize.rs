//! Compaction summarization.
//!
//! Serializes conversation history into a prompt for the summarizer
//! model and invokes the model with retry logic. Extracted from
//! `provider/mod.rs`.

use crate::session::{MessageRole, SessionMessage, ToolCallState};

use rig::streaming::StreamingChat;

/// Serialize the full conversation prefix for compaction summarization.
/// Returns a formatted string with all messages including tool calls
/// (args + results), truncated per-tool at 2KB for memory safety.
pub(crate) fn serialize_conversation(messages: &[SessionMessage]) -> String {
    let mut result = String::new();
    for msg in messages {
        let role_tag = match msg.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::System => "System",
        };
        result.push_str(&format!("[{}]: {}\n", role_tag, msg.content));
        for tc in &msg.tool_calls {
            let args_str = serde_json::to_string(&tc.args).unwrap_or_else(|_| "{}".to_string());
            result.push_str(&format!("[Tool: {}({})]\n", tc.name, args_str));
            match &tc.state {
                ToolCallState::Completed { result: out } => {
                    const PER_TOOL_CAP: usize = 2048;
                    if out.len() > PER_TOOL_CAP {
                        let trimmed: String = out.chars().take(PER_TOOL_CAP).collect();
                        result.push_str(&format!(
                            "[Result: {} ... (truncated, {} bytes total)]\n",
                            trimmed,
                            out.len()
                        ));
                    } else {
                        result.push_str(&format!("[Result: {}]\n", out));
                    }
                }
                ToolCallState::Interrupted => {
                    result.push_str("[Result: <interrupted>]\n");
                }
                ToolCallState::Failed { error } => {
                    result.push_str(&format!("[Result: <failed: {}>]\n", error));
                }
            }
        }
        result.push('\n');
    }
    result
}

/// Call the summarizer model with the full conversation prefix.
/// The summarizer is invoked by `/compress`, often exactly when the
/// user's context is about to overflow. Uses a retry loop with the
/// same `RecoveryPolicy` shape as the main agent.
pub(crate) async fn summarize_with_model(
    model: super::AnyModel,
    prompt: String,
) -> anyhow::Result<String> {
    match model {
        super::AnyModel::OpenRouter(m) => run_summarizer(m, prompt).await,
        super::AnyModel::OpenAI(m) => run_summarizer(m, prompt).await,
        super::AnyModel::Anthropic(m) => run_summarizer(m, prompt).await,
        super::AnyModel::Gemini(m) => run_summarizer(m, prompt).await,
        super::AnyModel::DeepSeek(m) => run_summarizer(m, prompt).await,
        super::AnyModel::Glm(m) => run_summarizer(m, prompt).await,
        super::AnyModel::Ollama(m) => run_summarizer(m, prompt).await,
        super::AnyModel::Custom(m) => run_summarizer(m, prompt).await,
    }
}

async fn run_summarizer<M>(model: M, prompt: String) -> anyhow::Result<String>
where
    M: rig::completion::CompletionModel + Clone + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
{
    use crate::agent::recovery::{self, RecoveryPolicy};
    let policy = RecoveryPolicy::default();
    let mut attempts: usize = 0;
    loop {
        let agent = rig::agent::AgentBuilder::new(model.clone())
            .preamble("You are a conversation summarizer.")
            .build();

        let mut stream = agent
            .stream_chat(prompt.clone(), Vec::<rig::completion::Message>::new())
            .multi_turn(1)
            .await;

        let mut response = String::new();
        let mut error: Option<String> = None;
        use futures::StreamExt;
        while let Some(item) = stream.next().await {
            match item {
                Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig::streaming::StreamedAssistantContent::Text(text),
                )) => response.push_str(&text.text),
                Ok(rig::agent::MultiTurnStreamItem::FinalResponse(res)) => {
                    response = res.response().to_string();
                    break;
                }
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
                _ => {}
            }
        }

        if let Some(msg) = error {
            let kind = recovery::classify_error(&msg);
            if policy.should_retry(attempts, kind) {
                let delay = policy.backoff_duration_for_msg(attempts, &msg);
                tracing::info!(
                    "summarizer retry {}/{} after {:?} ({:?}): {}",
                    attempts + 1,
                    policy.max_retries(),
                    delay,
                    kind,
                    msg
                );
                tokio::time::sleep(delay).await;
                attempts += 1;
                continue;
            }
            return Err(anyhow::anyhow!("Compression failed: {}", msg));
        }

        if response.is_empty() {
            anyhow::bail!("Compression returned empty response");
        }

        return Ok(response);
    }
}
