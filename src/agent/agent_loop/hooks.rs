//! `beforeToolCall` and `afterToolCall` config hooks.
//!
//! Faithful port of pi's hook surface at agent-loop.ts:578-708.
//!
//! Pi's hooks are JavaScript callbacks that receive a context
//! object and may MUTATE the args in place (test pi:310). Rust
//! can't compose `&mut` cleanly with `Pin<Box<dyn Future>>`, so
//! we pass `args` by value and return the (possibly mutated)
//! args alongside the hook result. The dispatcher threads the
//! returned args forward — semantically identical to pi's
//! mutate-in-place but with explicit data flow.

use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use super::message::AssistantMessage;
use super::result::{AfterToolCallResult, BeforeToolCallResult, LoopToolResult};

/// Context passed to `beforeToolCall`. Port of pi
/// `BeforeToolCallContext` (types.ts:84).
///
/// Fields are owned values (clones) so the hook closure can be
/// `Fn(Ctx) -> Future` rather than `Fn(&Ctx) -> Future` — the
/// latter is hairy with `Pin<Box<dyn Future>>` lifetimes. Pi's
/// hooks receive references to mutable JS objects; we trade a
/// small clone overhead for a clean async-fn shape.
#[derive(Debug, Clone)]
pub struct BeforeToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call_id: String,
    pub tool_call_name: String,
    /// Validated args. The hook may mutate these (via the
    /// returned `BeforeToolCallReturn.args`) — pi tests this at
    /// agent-loop.test.ts:310.
    pub args: Value,
}

/// Return value of `beforeToolCall`. Pi returns
/// `Promise<BeforeToolCallResult | undefined>` AND mutates the
/// context's `args` in place. Since Rust can't elegantly mutate
/// through a moved value, the closure returns BOTH the result
/// (possibly None) and the (possibly mutated) args.
#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallReturn {
    /// Pi's return value: `block?` + `reason?`. `None` means
    /// "let the call proceed unchanged".
    pub result: Option<BeforeToolCallResult>,
    /// Possibly-mutated args. Even when `result` is None, these
    /// args are what the tool executes with. Hooks that don't
    /// mutate should return the input args unchanged.
    pub args: Value,
}

/// `beforeToolCall` hook signature. Pi (types.ts:262):
///   `(context: BeforeToolCallContext, signal?) => Promise<BeforeToolCallResult | undefined>`
pub type BeforeToolCallFn = Arc<
    dyn Fn(BeforeToolCallContext) -> Pin<Box<dyn Future<Output = BeforeToolCallReturn> + Send>>
        + Send
        + Sync,
>;

/// Context passed to `afterToolCall`. Port of pi
/// `AfterToolCallContext` (types.ts:96).
#[derive(Debug, Clone)]
pub struct AfterToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call_id: String,
    pub tool_call_name: String,
    pub args: Value,
    pub result: LoopToolResult,
    pub is_error: bool,
}

/// `afterToolCall` hook signature. Pi (types.ts:276):
///   `(context: AfterToolCallContext, signal?) => Promise<AfterToolCallResult | undefined>`
///
/// Returning `None` keeps the executed result verbatim; returning
/// `Some(AfterToolCallResult { … })` overrides any of the four
/// fields per pi's merge semantics (content/details/isError/
/// terminate replace in full when Some).
pub type AfterToolCallFn = Arc<
    dyn Fn(
            AfterToolCallContext,
        ) -> Pin<Box<dyn Future<Output = Option<AfterToolCallResult>> + Send>>
        + Send
        + Sync,
>;

#[cfg(test)]
mod tests {
    use super::*;

    /// `BeforeToolCallReturn::default()` is the no-op outcome —
    /// result=None, args=Null. Hooks that "did nothing" return
    /// effectively this shape (with the input args instead of
    /// Null).
    #[test]
    fn before_return_default() {
        let r = BeforeToolCallReturn::default();
        assert!(r.result.is_none());
        assert_eq!(r.args, Value::Null);
    }
}
