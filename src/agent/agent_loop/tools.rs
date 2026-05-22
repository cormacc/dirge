//! Tool execution dispatcher. Phase 2 ports the SEQUENTIAL path
//! and the shared helpers (`prepare_tool_call`,
//! `execute_prepared_tool_call`, `finalize_executed_tool_call`,
//! `should_terminate_tool_batch`). The parallel path lands in
//! phase 3.
//!
//! Faithful port of pi `agent-loop.ts:370-737`. Each helper cites
//! its pi line range.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;

use super::hooks::{AfterToolCallContext, BeforeToolCallContext};
use super::message::{AssistantMessage, ContentBlock, LoopEvent, LoopMessage, ToolResultMessage};
use super::result::LoopToolResult;
use super::tool::{AbortSignal, LoopTool, LoopToolUpdate};
use super::types::{Context, LoopConfig};

/// Batch return shape. Port of pi `ExecutedToolCallBatch`
/// (agent-loop.ts:390-393).
#[derive(Debug, Clone)]
pub struct ExecutedToolCallBatch {
    /// Tool-result messages to append to the transcript. Order
    /// matches the source order of the assistant's `toolCall`
    /// blocks (pi: this is true for parallel via the
    /// `orderedFinalizedCalls` re-emit in source order at
    /// agent-loop.ts:506-510; for sequential the iteration order
    /// IS the source order).
    pub messages: Vec<ToolResultMessage>,

    /// Early-termination signal. Pi semantics: TRUE iff every
    /// finalized result has `terminate == true` AND the batch
    /// is non-empty (`shouldTerminateToolBatch` at line 544).
    pub terminate: bool,
}

/// One tool call extracted from an assistant message. Port of pi
/// `AgentToolCall` (types.ts:47). Concrete struct rather than
/// reference to keep the dispatcher's data flow plain.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Internal: outcome of `prepare_tool_call`. Port of pi's
/// `PreparedToolCall | ImmediateToolCallOutcome` union
/// (agent-loop.ts:518-540).
enum PrepareOutcome {
    /// Tool found and validated; ready for execute.
    Prepared {
        tool: Arc<dyn LoopTool>,
        args: Value,
    },
    /// Short-circuit error: tool missing, schema rejected,
    /// signal aborted, or beforeToolCall blocked.
    Immediate {
        result: LoopToolResult,
        is_error: bool,
    },
}

/// Internal: outcome of `execute_prepared_tool_call`. Port of pi
/// `ExecutedToolCallOutcome` (agent-loop.ts:531-534).
struct ExecutedOutcome {
    result: LoopToolResult,
    is_error: bool,
}

/// Internal: outcome of `finalize_executed_tool_call`. Port of pi
/// `FinalizedToolCallOutcome` (agent-loop.ts:536-540).
#[derive(Debug, Clone)]
struct FinalizedOutcome {
    tool_call: ToolCall,
    result: LoopToolResult,
    is_error: bool,
}

/// Execute a batch of tool calls SEQUENTIALLY. Faithful port of
/// pi `executeToolCallsSequential` (agent-loop.ts:395-449).
///
/// Per-iteration:
///   1. emit `tool_execution_start`
///   2. prepare (lookup + prepareArguments + validate + before)
///   3. execute (if prepared) + finalize (afterToolCall)
///   4. emit `tool_execution_end`
///   5. emit `message_start` / `message_end` for the tool-result
///      message
///   6. if signal aborted: break
pub async fn execute_tool_calls_sequential(
    context: &Context,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &LoopConfig,
    signal: &AbortSignal,
    emit: &mpsc::Sender<LoopEvent>,
) -> ExecutedToolCallBatch {
    let mut finalized_calls: Vec<FinalizedOutcome> = Vec::with_capacity(tool_calls.len());
    let mut messages: Vec<ToolResultMessage> = Vec::with_capacity(tool_calls.len());

    for tool_call in tool_calls {
        // 1. tool_execution_start
        let _ = emit
            .send(LoopEvent::ToolExecutionStart {
                tool_call_id: tool_call.id.clone(),
                tool_name: tool_call.name.clone(),
                args: tool_call.arguments.clone(),
            })
            .await;

        // 2. prepare
        let prepared =
            prepare_tool_call(context, assistant_message, tool_call, config, signal).await;

        // 3. execute + finalize
        let finalized = match prepared {
            PrepareOutcome::Immediate { result, is_error } => FinalizedOutcome {
                tool_call: tool_call.clone(),
                result,
                is_error,
            },
            PrepareOutcome::Prepared { tool, args } => {
                let executed =
                    execute_prepared_tool_call(&tool, tool_call, &args, signal, emit).await;
                finalize_executed_tool_call(
                    context,
                    assistant_message,
                    tool_call,
                    &args,
                    executed,
                    config,
                )
                .await
            }
        };

        // 4. tool_execution_end
        emit_tool_execution_end(&finalized, emit).await;

        // 5. tool-result message
        let result_msg = create_tool_result_message(&finalized);
        emit_tool_result_message(&result_msg, emit).await;

        finalized_calls.push(finalized);
        messages.push(result_msg);

        // 6. honor signal
        if signal.is_cancelled() {
            break;
        }
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}

/// Lookup tool, run `prepareArguments`, validate (TODO phase 3),
/// run `beforeToolCall`. Faithful port of pi `prepareToolCall`
/// (agent-loop.ts:562-626).
///
/// Important deviation from pi: phase 2 does NOT JSON-schema-
/// validate args. Pi calls `validateToolArguments(tool, toolCall)`
/// at line 580; we skip that step because dirge has no embedded
/// JSON-Schema validator (rig tools self-parse via serde). A
/// future phase can add a validator if a real schema-mismatch
/// case surfaces — for now any deserialization mismatch surfaces
/// from the tool's `execute` as a normal error.
async fn prepare_tool_call(
    context: &Context,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    config: &LoopConfig,
    signal: &AbortSignal,
) -> PrepareOutcome {
    // Find the tool by name. Pi line 569.
    let tool = match context.tools.iter().find(|t| t.name() == tool_call.name) {
        Some(t) => t.clone(),
        None => {
            return PrepareOutcome::Immediate {
                result: create_error_tool_result(&format!("Tool {} not found", tool_call.name)),
                is_error: true,
            };
        }
    };

    // prepareArguments compat shim. Pi line 579.
    let prepared_args = tool.prepare_arguments(tool_call.arguments.clone());

    // Schema validate — DEFERRED. See doc above. Pi line 580.
    let mut validated_args = prepared_args;

    // beforeToolCall. Pi lines 581-605.
    if let Some(hook) = &config.before_tool_call {
        let hook_ctx = BeforeToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call_id: tool_call.id.clone(),
            tool_call_name: tool_call.name.clone(),
            args: validated_args.clone(),
        };
        let ret = hook(hook_ctx).await;
        // The hook may mutate args via the returned `args` field.
        // Thread it forward to execute. Pi mutates in-place; we
        // pass by value (documented in hooks.rs).
        validated_args = ret.args;

        if signal.is_cancelled() {
            return PrepareOutcome::Immediate {
                result: create_error_tool_result("Operation aborted"),
                is_error: true,
            };
        }
        if let Some(before_result) = ret.result
            && before_result.block.unwrap_or(false)
        {
            let reason = before_result
                .reason
                .unwrap_or_else(|| "Tool execution was blocked".to_string());
            return PrepareOutcome::Immediate {
                result: create_error_tool_result(&reason),
                is_error: true,
            };
        }
    }

    // Final signal check before returning prepared. Pi lines
    // 606-612.
    if signal.is_cancelled() {
        return PrepareOutcome::Immediate {
            result: create_error_tool_result("Operation aborted"),
            is_error: true,
        };
    }

    PrepareOutcome::Prepared {
        tool,
        args: validated_args,
    }
}

/// Execute a prepared tool call. Faithful port of pi
/// `executePreparedToolCall` (agent-loop.ts:628-663).
///
/// The tool's `on_update` callback emits `tool_execution_update`
/// events. Pi awaits all the update emits via
/// `Promise.all(updateEvents)`; we let them flow into the mpsc
/// channel as the tool calls them (`send().await` orders writes
/// per-channel anyway).
async fn execute_prepared_tool_call(
    tool: &Arc<dyn LoopTool>,
    tool_call: &ToolCall,
    args: &Value,
    signal: &AbortSignal,
    emit: &mpsc::Sender<LoopEvent>,
) -> ExecutedOutcome {
    // Build the on_update callback. Pi captures these via
    // `updateEvents` promise list (agent-loop.ts:633, 641-652).
    // We forward directly through the mpsc channel — same
    // ordering semantics since tokio channels are FIFO.
    let emit_clone = emit.clone();
    let id_clone = tool_call.id.clone();
    let name_clone = tool_call.name.clone();
    let args_clone = tool_call.arguments.clone();
    let on_update: LoopToolUpdate = Arc::new(move |partial: &LoopToolResult| {
        // `try_send` rather than `.send().await` because the
        // callback is sync — pi's callback is sync too. If the
        // channel is closed/full, drop the update (matches
        // dirge's bounded-channel philosophy from earlier
        // notification work).
        let _ = emit_clone.try_send(LoopEvent::ToolExecutionUpdate {
            tool_call_id: id_clone.clone(),
            tool_name: name_clone.clone(),
            args: args_clone.clone(),
            partial_result: partial.clone(),
        });
    });

    match tool
        .execute(&tool_call.id, args.clone(), signal.clone(), on_update)
        .await
    {
        Ok(result) => ExecutedOutcome {
            result,
            is_error: false,
        },
        Err(err) => ExecutedOutcome {
            result: create_error_tool_result(&err),
            is_error: true,
        },
    }
}

/// Finalize an executed tool result via `afterToolCall`. Faithful
/// port of pi `finalizeExecutedToolCall` (agent-loop.ts:665-708).
///
/// Merge semantics (pi lines 689-695): each Some field of
/// `AfterToolCallResult` REPLACES the executed result's
/// corresponding field IN FULL. Omitted (None) fields keep the
/// original.
async fn finalize_executed_tool_call(
    context: &Context,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    args: &Value,
    executed: ExecutedOutcome,
    config: &LoopConfig,
) -> FinalizedOutcome {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(hook) = &config.after_tool_call {
        let hook_ctx = AfterToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call_id: tool_call.id.clone(),
            tool_call_name: tool_call.name.clone(),
            args: args.clone(),
            result: result.clone(),
            is_error,
        };
        // Pi catches hook errors and turns them into an error
        // tool result (agent-loop.ts:697-700). Our hook signature
        // doesn't have a Result return — closures that want to
        // signal errors do so via the `is_error` field. If a
        // future hook impl needs throw-and-catch behaviour we
        // extend the signature.
        if let Some(after) = hook(hook_ctx).await {
            result = LoopToolResult {
                content: after.content.unwrap_or(result.content),
                details: after.details.unwrap_or(result.details),
                terminate: after.terminate.or(result.terminate),
            };
            is_error = after.is_error.unwrap_or(is_error);
        }
    }

    // `context` is unused for now (pi passes it for symmetry with
    // beforeToolCall). Marker-binding to silence the warning until
    // a future hook impl uses it.
    let _ = context;

    FinalizedOutcome {
        tool_call: tool_call.clone(),
        result,
        is_error,
    }
}

/// `shouldTerminateToolBatch`: empty batch → false; otherwise
/// true iff EVERY result has `terminate == true`. Faithful port
/// of pi line 544.
fn should_terminate_tool_batch(finalized: &[FinalizedOutcome]) -> bool {
    !finalized.is_empty()
        && finalized
            .iter()
            .all(|f| f.result.terminate.unwrap_or(false))
}

/// Build the "tool not found" / "operation aborted" / "blocked"
/// error result. Port of pi `createErrorToolResult` (line 710).
fn create_error_tool_result(message: &str) -> LoopToolResult {
    LoopToolResult {
        content: vec![serde_json::json!({"type": "text", "text": message})],
        details: serde_json::json!({}),
        terminate: None,
    }
}

/// Emit the `tool_execution_end` event. Port of pi line 717.
async fn emit_tool_execution_end(finalized: &FinalizedOutcome, emit: &mpsc::Sender<LoopEvent>) {
    let _ = emit
        .send(LoopEvent::ToolExecutionEnd {
            tool_call_id: finalized.tool_call.id.clone(),
            tool_name: finalized.tool_call.name.clone(),
            result: finalized.result.clone(),
            is_error: finalized.is_error,
        })
        .await;
}

/// Build the `ToolResultMessage` artifact appended to the
/// transcript. Port of pi `createToolResultMessage` (line 727).
fn create_tool_result_message(finalized: &FinalizedOutcome) -> ToolResultMessage {
    // Pi shape: { role, toolCallId, toolName, content, details,
    // isError, timestamp }. Our LoopToolResult.content is
    // `Vec<Value>` (the raw blocks pi calls TextContent /
    // ImageContent); we need to map them to ContentBlock for the
    // message. Phase 1 represented blocks as either typed
    // ContentBlock variants OR raw Value depending on the path;
    // phase 2 unifies via a best-effort parse: if a block has
    // `type: "text"` we recognise it, else we wrap as raw text
    // with debug string.
    let content_blocks: Vec<ContentBlock> = finalized
        .result
        .content
        .iter()
        .map(content_value_to_block)
        .collect();

    ToolResultMessage {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: content_blocks,
        details: finalized.result.details.clone(),
        is_error: finalized.is_error,
    }
}

fn content_value_to_block(value: &Value) -> ContentBlock {
    // Recognise pi's `{type: "text", text: "..."}` shape.
    if let Some(obj) = value.as_object()
        && obj.get("type").and_then(|t| t.as_str()) == Some("text")
        && let Some(text) = obj.get("text").and_then(|t| t.as_str())
    {
        return ContentBlock::Text {
            text: text.to_string(),
        };
    }
    // Fallback: stringify the value. Better than dropping data.
    ContentBlock::Text {
        text: value.to_string(),
    }
}

/// Emit the message_start + message_end pair for the tool-result
/// message. Port of pi `emitToolResultMessage` (line 739).
async fn emit_tool_result_message(msg: &ToolResultMessage, emit: &mpsc::Sender<LoopEvent>) {
    let _ = emit
        .send(LoopEvent::MessageStart {
            message: LoopMessage::ToolResult(msg.clone()),
        })
        .await;
    let _ = emit
        .send(LoopEvent::MessageEnd {
            message: LoopMessage::ToolResult(msg.clone()),
        })
        .await;
}

/// Extract `ToolCall`s from an assistant message's content. Port
/// of pi line 380 `message.content.filter((c) => c.type ===
/// "toolCall")` adapted to our typed enum.
pub fn extract_tool_calls(msg: &AssistantMessage) -> Vec<ToolCall> {
    msg.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
}

// =====================================================================
// Tests — ported from pi/test/agent-loop.test.ts
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::agent_loop::hooks::{BeforeToolCallFn, BeforeToolCallReturn};
    use crate::agent::agent_loop::message::{ContentBlock, StopReason};
    use crate::agent::agent_loop::result::{AfterToolCallResult, BeforeToolCallResult};
    use crate::agent::agent_loop::types::{ConvertToLlmFn, ToolExecutionMode};
    use std::pin::Pin;
    use std::sync::Mutex;

    /// Mock LoopTool that records its calls and returns a canned
    /// result. Used by phase-2 tests in lieu of a real rig tool.
    struct EchoTool {
        name: String,
        /// Set by tests to control whether `prepare_arguments`
        /// mutates the input shape (pi test 372).
        prepare_arguments_fn: Option<Box<dyn Fn(Value) -> Value + Send + Sync>>,
        /// Set by tests to override `execution_mode`.
        execution_mode: Option<ToolExecutionMode>,
        /// Set by tests to inject `terminate: true` into every
        /// result (pi test 1067).
        terminate: bool,
        /// Recorded args passed to `execute` (so tests can
        /// assert mutations from beforeToolCall took effect).
        executed_args: Arc<Mutex<Vec<Value>>>,
    }

    impl std::fmt::Debug for EchoTool {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("EchoTool")
                .field("name", &self.name)
                .field("execution_mode", &self.execution_mode)
                .field("terminate", &self.terminate)
                .finish()
        }
    }

    impl EchoTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                prepare_arguments_fn: None,
                execution_mode: None,
                terminate: false,
                executed_args: Arc::new(Mutex::new(Vec::new())),
            }
        }
        fn with_prepare(mut self, f: impl Fn(Value) -> Value + Send + Sync + 'static) -> Self {
            self.prepare_arguments_fn = Some(Box::new(f));
            self
        }
        fn with_terminate(mut self) -> Self {
            self.terminate = true;
            self
        }
    }

    impl LoopTool for EchoTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "Echo tool"
        }
        fn label(&self) -> &str {
            "Echo"
        }
        fn parameters(&self) -> &Value {
            // Phase 2 doesn't validate; an empty object is fine.
            static EMPTY: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
            EMPTY.get_or_init(|| serde_json::json!({"type": "object"}))
        }
        fn execution_mode(&self) -> Option<ToolExecutionMode> {
            self.execution_mode
        }
        fn prepare_arguments(&self, args: Value) -> Value {
            if let Some(f) = &self.prepare_arguments_fn {
                f(args)
            } else {
                args
            }
        }
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            args: Value,
            _signal: AbortSignal,
            _on_update: LoopToolUpdate,
        ) -> Pin<Box<dyn Future<Output = Result<LoopToolResult, String>> + Send + 'a>> {
            let recorded = self.executed_args.clone();
            let terminate = self.terminate;
            Box::pin(async move {
                recorded.lock().unwrap().push(args.clone());
                let text = format!("echoed: {}", args);
                Ok(LoopToolResult {
                    content: vec![serde_json::json!({"type": "text", "text": text})],
                    details: args,
                    terminate: if terminate { Some(true) } else { None },
                })
            })
        }
    }

    fn identity_converter() -> ConvertToLlmFn {
        Arc::new(|messages: &[Value]| messages.to_vec())
    }

    fn build_config() -> LoopConfig {
        LoopConfig {
            convert_to_llm: identity_converter(),
            transform_context: None,
            get_api_key: None,
            api_key: None,
            tool_execution: ToolExecutionMode::Sequential,
            before_tool_call: None,
            after_tool_call: None,
        }
    }

    fn build_context(tool: Arc<dyn LoopTool>) -> Context {
        Context {
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: vec![tool],
        }
    }

    /// Port of pi test "should handle tool calls and results"
    /// (agent-loop.test.ts:239). Phase-2 scope: verify the
    /// sequential dispatcher actually invokes the tool, emits
    /// the expected lifecycle events, and produces a non-error
    /// tool-result message. The full agent-loop flow (assistant
    /// turn → tool → next assistant turn) is verified in phase 4.
    #[tokio::test]
    async fn test_handle_tool_calls_and_results() {
        let echo = Arc::new(EchoTool::new("echo"));
        let context = build_context(echo.clone());
        let assistant_msg = AssistantMessage::new(
            vec![ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({"value": "hello"}),
            }],
            StopReason::ToolUse,
        );
        let tool_calls = extract_tool_calls(&assistant_msg);
        assert_eq!(tool_calls.len(), 1);

        let (tx, mut rx) = mpsc::channel::<LoopEvent>(64);
        let config = build_config();
        let signal = AbortSignal::new();

        let batch = execute_tool_calls_sequential(
            &context,
            &assistant_msg,
            &tool_calls,
            &config,
            &signal,
            &tx,
        )
        .await;
        drop(tx);

        // Tool executed; args reached `execute`.
        let recorded = echo.executed_args.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["value"], "hello");
        drop(recorded);

        // Batch shape: one non-error message; not terminating.
        assert_eq!(batch.messages.len(), 1);
        assert!(!batch.messages[0].is_error);
        assert!(!batch.terminate);

        // Event sequence: tool_execution_start →
        // tool_execution_end → message_start (toolResult) →
        // message_end (toolResult).
        let mut kinds = Vec::new();
        while let Some(e) = rx.recv().await {
            kinds.push(e.kind().to_string());
        }
        assert_eq!(
            kinds,
            vec![
                "tool_execution_start",
                "tool_execution_end",
                "message_start",
                "message_end",
            ]
        );
    }

    /// Port of pi test "should execute mutated beforeToolCall
    /// args without revalidation" (agent-loop.test.ts:310). The
    /// before-hook mutates `args.value` to a new value; the tool
    /// must see the mutated args.
    #[tokio::test]
    async fn test_before_tool_call_mutates_args() {
        let echo = Arc::new(EchoTool::new("echo"));
        let context = build_context(echo.clone());
        let assistant_msg = AssistantMessage::new(
            vec![ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({"value": "hello"}),
            }],
            StopReason::ToolUse,
        );
        let tool_calls = extract_tool_calls(&assistant_msg);

        // Hook: replace args.value with 123.
        let before: BeforeToolCallFn = Arc::new(|ctx: BeforeToolCallContext| {
            Box::pin(async move {
                let mut args = ctx.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("value".to_string(), serde_json::json!(123));
                }
                BeforeToolCallReturn { result: None, args }
            })
        });
        let mut config = build_config();
        config.before_tool_call = Some(before);

        let (tx, mut rx) = mpsc::channel::<LoopEvent>(64);
        let signal = AbortSignal::new();
        let _ = execute_tool_calls_sequential(
            &context,
            &assistant_msg,
            &tool_calls,
            &config,
            &signal,
            &tx,
        )
        .await;
        drop(tx);
        while rx.recv().await.is_some() {}

        // The tool must have observed the MUTATED args.
        let recorded = echo.executed_args.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["value"], serde_json::json!(123));
    }

    /// Port of pi test "should prepare tool arguments for
    /// validation" (agent-loop.test.ts:372). The
    /// `prepare_arguments` shim transforms the raw provider args
    /// `{oldText, newText}` into the schema-shape
    /// `{edits: [{oldText, newText}]}` before the tool executes.
    #[tokio::test]
    async fn test_prepare_arguments_shim() {
        let edit = Arc::new(EchoTool::new("edit").with_prepare(|args: Value| {
            // Pi-faithful: if input has oldText+newText at the
            // top level, wrap into `{edits: [{oldText, newText}]}`.
            if let Some(obj) = args.as_object()
                && obj.contains_key("oldText")
                && obj.contains_key("newText")
            {
                return serde_json::json!({
                    "edits": [{
                        "oldText": obj.get("oldText").unwrap(),
                        "newText": obj.get("newText").unwrap(),
                    }]
                });
            }
            args
        }));
        let context = build_context(edit.clone());
        let assistant_msg = AssistantMessage::new(
            vec![ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "edit".to_string(),
                arguments: serde_json::json!({"oldText": "before", "newText": "after"}),
            }],
            StopReason::ToolUse,
        );
        let tool_calls = extract_tool_calls(&assistant_msg);

        let (tx, mut rx) = mpsc::channel::<LoopEvent>(64);
        let config = build_config();
        let signal = AbortSignal::new();
        let _ = execute_tool_calls_sequential(
            &context,
            &assistant_msg,
            &tool_calls,
            &config,
            &signal,
            &tx,
        )
        .await;
        drop(tx);
        while rx.recv().await.is_some() {}

        let recorded = edit.executed_args.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        let edits = recorded[0].get("edits").expect("shim should produce edits");
        let arr = edits.as_array().expect("edits is array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["oldText"], "before");
        assert_eq!(arr[0]["newText"], "after");
    }

    /// Phase-2 scope of pi test "should stop after a tool batch
    /// when every tool result sets terminate=true"
    /// (agent-loop.test.ts:1067). Pi's test verifies the LOOP
    /// stops; phase 2 verifies the DISPATCHER returns
    /// `terminate: true`. Loop-level verification lands in
    /// phase 4 when the loop drives the dispatcher.
    #[tokio::test]
    async fn test_dispatcher_terminate_when_all_results_terminate() {
        let echo = Arc::new(EchoTool::new("echo").with_terminate());
        let context = build_context(echo.clone());
        let assistant_msg = AssistantMessage::new(
            vec![ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            }],
            StopReason::ToolUse,
        );
        let tool_calls = extract_tool_calls(&assistant_msg);
        let (tx, _rx) = mpsc::channel::<LoopEvent>(64);
        let config = build_config();
        let signal = AbortSignal::new();
        let batch = execute_tool_calls_sequential(
            &context,
            &assistant_msg,
            &tool_calls,
            &config,
            &signal,
            &tx,
        )
        .await;
        assert!(
            batch.terminate,
            "single terminate=true should set batch.terminate"
        );
    }

    /// Phase-2 scope of pi test "should allow afterToolCall to
    /// mark a tool batch as terminating" (agent-loop.test.ts:1184).
    /// afterToolCall returns `{ terminate: true }` even though
    /// the underlying tool didn't set terminate; the override
    /// propagates.
    #[tokio::test]
    async fn test_after_tool_call_can_set_terminate() {
        let echo = Arc::new(EchoTool::new("echo")); // no inherent terminate
        let context = build_context(echo);
        let assistant_msg = AssistantMessage::new(
            vec![ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            }],
            StopReason::ToolUse,
        );
        let tool_calls = extract_tool_calls(&assistant_msg);

        let after: crate::agent::agent_loop::hooks::AfterToolCallFn = Arc::new(|_ctx| {
            Box::pin(async move {
                Some(AfterToolCallResult {
                    content: None,
                    details: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        });
        let mut config = build_config();
        config.after_tool_call = Some(after);

        let (tx, _rx) = mpsc::channel::<LoopEvent>(64);
        let signal = AbortSignal::new();
        let batch = execute_tool_calls_sequential(
            &context,
            &assistant_msg,
            &tool_calls,
            &config,
            &signal,
            &tx,
        )
        .await;
        assert!(
            batch.terminate,
            "afterToolCall override should mark batch terminating"
        );
    }

    /// Tool not found → immediate error result. Port of pi
    /// `prepareToolCall` line 569-576 — the "Tool X not found"
    /// short-circuit.
    #[tokio::test]
    async fn test_tool_not_found_immediate_error() {
        let echo = Arc::new(EchoTool::new("echo"));
        let context = build_context(echo);
        let assistant_msg = AssistantMessage::new(
            vec![ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "nonexistent".to_string(),
                arguments: serde_json::json!({}),
            }],
            StopReason::ToolUse,
        );
        let tool_calls = extract_tool_calls(&assistant_msg);

        let (tx, _rx) = mpsc::channel::<LoopEvent>(64);
        let config = build_config();
        let signal = AbortSignal::new();
        let batch = execute_tool_calls_sequential(
            &context,
            &assistant_msg,
            &tool_calls,
            &config,
            &signal,
            &tx,
        )
        .await;
        assert_eq!(batch.messages.len(), 1);
        assert!(batch.messages[0].is_error);
        // Error message contains the missing-tool name.
        match &batch.messages[0].content[0] {
            ContentBlock::Text { text } => assert!(
                text.contains("nonexistent"),
                "error text should name the missing tool: {text}"
            ),
            _ => panic!("expected text content block"),
        }
    }

    /// beforeToolCall block=true → immediate error with reason.
    /// Port of pi `prepareToolCall` lines 598-604.
    #[tokio::test]
    async fn test_before_tool_call_block_with_reason() {
        let echo = Arc::new(EchoTool::new("echo"));
        let context = build_context(echo.clone());
        let assistant_msg = AssistantMessage::new(
            vec![ContentBlock::ToolCall {
                id: "tool-1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            }],
            StopReason::ToolUse,
        );
        let tool_calls = extract_tool_calls(&assistant_msg);

        let before: BeforeToolCallFn = Arc::new(|ctx: BeforeToolCallContext| {
            Box::pin(async move {
                BeforeToolCallReturn {
                    result: Some(BeforeToolCallResult {
                        block: Some(true),
                        reason: Some("policy violation".to_string()),
                    }),
                    args: ctx.args,
                }
            })
        });
        let mut config = build_config();
        config.before_tool_call = Some(before);

        let (tx, _rx) = mpsc::channel::<LoopEvent>(64);
        let signal = AbortSignal::new();
        let batch = execute_tool_calls_sequential(
            &context,
            &assistant_msg,
            &tool_calls,
            &config,
            &signal,
            &tx,
        )
        .await;

        // Tool never executed.
        assert!(echo.executed_args.lock().unwrap().is_empty());
        // Result is an error with our reason text.
        assert!(batch.messages[0].is_error);
        match &batch.messages[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("policy violation"), "got: {text}");
            }
            _ => panic!("expected text content block"),
        }
    }

    /// `should_terminate_tool_batch` invariants:
    ///   - empty batch → false
    ///   - some terminate=false → false
    ///   - all terminate=true → true
    /// Faithful port of pi line 544.
    #[test]
    fn should_terminate_invariants() {
        let make = |terminate: Option<bool>| FinalizedOutcome {
            tool_call: ToolCall {
                id: "x".into(),
                name: "x".into(),
                arguments: Value::Null,
            },
            result: LoopToolResult {
                content: vec![],
                details: Value::Null,
                terminate,
            },
            is_error: false,
        };
        assert!(!should_terminate_tool_batch(&[]));
        assert!(!should_terminate_tool_batch(&[make(Some(false))]));
        assert!(!should_terminate_tool_batch(&[make(None)]));
        assert!(!should_terminate_tool_batch(&[
            make(Some(true)),
            make(Some(false))
        ]));
        assert!(should_terminate_tool_batch(&[make(Some(true))]));
        assert!(should_terminate_tool_batch(&[
            make(Some(true)),
            make(Some(true)),
        ]));
    }
}
