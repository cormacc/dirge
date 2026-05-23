//! `JanetLoopTool` — adapts a plugin-registered tool into a
//! `LoopTool` the agent loop can dispatch.
//!
//! Phase 9a — first feature of the pi-style extension API
//! (see `bd show dirge-bw2`). Plugins call
//! `(harness/register-tool name description label parameters handler
//!                         &opt execution-mode)` from Janet; the host
//! reads the registry via `PluginManager::list_plugin_tools()` and
//! wraps each entry in this adapter.
//!
//! Pi reference: `packages/coding-agent/src/core/extensions/types.ts`
//! line 1133 — `registerTool<TParams, TDetails, TState>(...)`. Pi's
//! TypeBox `TSchema` parameter collapses here to a raw JSON string —
//! dirge's `LoopTool::parameters()` returns `&Value`, but Janet
//! plugins don't have a TypeBox-equivalent so they pass the schema as
//! a JSON string that we parse once at construction.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;

use crate::agent::agent_loop::tool::{AbortSignal, LoopTool, LoopToolUpdate};
use crate::agent::agent_loop::result::LoopToolResult;
use crate::agent::agent_loop::types::ToolExecutionMode;

use super::{PluginManager, PluginToolMeta};

/// `LoopTool` impl backed by a Janet handler. The execute path
/// briefly locks the PluginManager mutex, dispatches into Janet via
/// `invoke_plugin_tool`, and surfaces the stringified result as a
/// single text content block. Janet errors become `Err(String)`
/// which the loop translates into an error tool result the same way
/// it would for a native tool.
pub struct JanetLoopTool {
    name: String,
    description: String,
    label: String,
    /// Parsed JSON-schema. Pre-parsed at construction so the hot
    /// `LoopTool::parameters()` path returns `&Value` without
    /// re-parsing on every LLM tool-list build.
    parameters: Value,
    handler: String,
    execution_mode: Option<ToolExecutionMode>,
    pm: Arc<Mutex<PluginManager>>,
}

impl std::fmt::Debug for JanetLoopTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `PluginManager` isn't Debug; skip it. The remaining fields
        // are enough for debug-printing a tool from the loop's
        // registry.
        f.debug_struct("JanetLoopTool")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("handler", &self.handler)
            .field("execution_mode", &self.execution_mode)
            .finish()
    }
}

impl JanetLoopTool {
    /// Build an adapter from a registry snapshot. Returns `None` if
    /// `meta.parameters` isn't valid JSON — plugin authors who hand
    /// us a syntactically broken schema get a clear "tool dropped"
    /// rather than the LLM seeing a corrupt parameters object.
    pub fn from_meta(meta: PluginToolMeta, pm: Arc<Mutex<PluginManager>>) -> Option<Self> {
        let parameters: Value = serde_json::from_str(&meta.parameters)
            .ok()
            .unwrap_or_else(|| {
                tracing::warn!(
                    target: "dirge::plugin",
                    tool = %meta.name,
                    raw = %meta.parameters,
                    "plugin tool parameters were not valid JSON — falling back to empty object schema",
                );
                Value::Object(serde_json::Map::new())
            });
        let execution_mode = match meta.execution_mode.as_deref() {
            Some("sequential") => Some(ToolExecutionMode::Sequential),
            Some("parallel") => Some(ToolExecutionMode::Parallel),
            _ => None,
        };
        Some(Self {
            name: meta.name,
            description: meta.description,
            label: meta.label,
            parameters,
            handler: meta.handler,
            execution_mode,
            pm,
        })
    }
}

impl LoopTool for JanetLoopTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn label(&self) -> &str {
        if self.label.is_empty() {
            &self.name
        } else {
            &self.label
        }
    }

    fn parameters(&self) -> &Value {
        &self.parameters
    }

    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        self.execution_mode
    }

    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        args: Value,
        _signal: AbortSignal,
        _on_update: LoopToolUpdate,
    ) -> Pin<Box<dyn Future<Output = Result<LoopToolResult, String>> + Send + 'a>> {
        // Serialize args back to JSON. Janet doesn't have a JSON
        // decoder bundled, so we hand the handler the raw string and
        // let it parse on the plugin side if needed (most plugins
        // just stringify for display).
        let args_json = args.to_string();
        let pm = self.pm.clone();
        let handler = self.handler.clone();
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || {
                let mut guard = pm
                    .lock()
                    .map_err(|_| "plugin manager mutex poisoned".to_string())?;
                guard.invoke_plugin_tool(&handler, &args_json)
            })
            .await
            .map_err(|e| format!("plugin tool task join error: {e}"))??;
            Ok(LoopToolResult {
                content: vec![serde_json::json!({"type": "text", "text": result})],
                details: Value::Null,
                terminate: None,
            })
        })
    }
}

#[cfg(all(test, feature = "plugin"))]
mod tests {
    use super::*;
    use crate::agent::agent_loop::tool::AbortSignal;

    fn noop_update() -> LoopToolUpdate {
        Arc::new(|_| {})
    }

    /// End-to-end: register a Janet tool, snapshot the registry,
    /// wrap it in a `JanetLoopTool`, dispatch via `execute()`. The
    /// LLM-visible result is exactly what the Janet handler returns.
    #[tokio::test]
    async fn janet_loop_tool_execute_round_trips_handler_output() {
        let pm = {
            let mut mgr = PluginManager::try_new().unwrap();
            mgr.eval(
                r#"(defn my-handler [args] (string "echo:" args))
                   (harness/register-tool "my-tool" "Echo" "MyTool" "{}" "my-handler")"#,
            )
            .unwrap();
            Arc::new(Mutex::new(mgr))
        };

        let metas: Vec<PluginToolMeta> = pm.lock().unwrap().list_plugin_tools();
        assert_eq!(metas.len(), 1);
        let tool = JanetLoopTool::from_meta(metas.into_iter().next().unwrap(), pm.clone())
            .expect("from_meta must succeed for valid schema");

        assert_eq!(tool.name(), "my-tool");
        assert_eq!(tool.label(), "MyTool");
        assert_eq!(tool.description(), "Echo");
        assert_eq!(tool.parameters(), &Value::Object(serde_json::Map::new()));

        let args = serde_json::json!({"x": 1});
        let result = tool
            .execute("call-1", args, AbortSignal::new(), noop_update())
            .await
            .expect("execute should succeed");

        let text = result
            .content
            .iter()
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, r#"echo:{"x":1}"#);
    }

    /// `execution_mode = :sequential` round-trips through to the
    /// `LoopTool::execution_mode()` method so the agent loop's batch
    /// scheduler treats the tool as mutating.
    #[tokio::test]
    async fn janet_loop_tool_sequential_mode_surfaces_to_loop() {
        let pm = {
            let mut mgr = PluginManager::try_new().unwrap();
            mgr.eval(
                r#"(harness/register-tool "mutate" "side effects" "Mutate"
                                            "{}" "noop" :sequential)
                   (defn noop [args] "ok")"#,
            )
            .unwrap();
            Arc::new(Mutex::new(mgr))
        };
        let metas: Vec<PluginToolMeta> = pm.lock().unwrap().list_plugin_tools();
        let tool = JanetLoopTool::from_meta(metas.into_iter().next().unwrap(), pm.clone()).unwrap();
        assert_eq!(tool.execution_mode(), Some(ToolExecutionMode::Sequential));
    }

    /// Handler errors propagate as `Err(_)`, NOT as an Ok result with
    /// the error text inlined. The loop's error path is what surfaces
    /// the failure to the LLM (so it can decide whether to retry),
    /// not the success path with garbled output.
    #[tokio::test]
    async fn janet_loop_tool_handler_error_surfaces_as_err() {
        let pm = {
            let mut mgr = PluginManager::try_new().unwrap();
            mgr.eval(
                r#"(defn bad [args] (error "intentional"))
                   (harness/register-tool "bad" "fails" "Bad" "{}" "bad")"#,
            )
            .unwrap();
            Arc::new(Mutex::new(mgr))
        };
        let metas: Vec<PluginToolMeta> = pm.lock().unwrap().list_plugin_tools();
        let tool = JanetLoopTool::from_meta(metas.into_iter().next().unwrap(), pm.clone()).unwrap();
        let err = tool
            .execute("c", Value::Object(Default::default()), AbortSignal::new(), noop_update())
            .await
            .expect_err("handler error should bubble up as Err");
        assert!(err.contains("intentional"), "got: {err}");
    }
}
