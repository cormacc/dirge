use bytes::Bytes;
use rig::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};

/// The Anthropic OAuth (subscription) wire path requires that requests carry
/// `Authorization: Bearer <token>` and NO `x-api-key`, and that the system
/// prompt begins with Claude Code's identifying block. rig 0.37's Anthropic
/// client always emits `x-api-key` from its api-key slot and exposes no
/// per-request header seam, so we normalize the outgoing request at the
/// transport boundary instead of forking rig-core.
// `bearer_token` is `Option` purely so a `Default` instance exists: rig 0.37's
// `CompletionModel` impl requires `HttpClientExt: Default`, but a
// default-constructed client is only ever a type-level placeholder and never
// actually sends a request. Real clients are built via `new`.
#[derive(Clone, Default)]
pub(crate) struct AnthropicHttpClient {
    inner: reqwest::Client,
    bearer_token: Option<String>,
}

// Hand-written so the OAuth access token can never leak into a log or panic
// message via `{:?}`.
impl std::fmt::Debug for AnthropicHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicHttpClient")
            .field("bearer_token", &"<redacted>")
            .finish()
    }
}

/// Anthropic requires this exact text as the first system block when
/// authenticating with a Claude Code OAuth token.
const CLAUDE_CODE_SYSTEM_PROMPT: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Beta flags Anthropic's API requires for the Claude Code OAuth wire path.
const ANTHROPIC_OAUTH_BETA: &str = "claude-code-20250219,oauth-2025-04-20";

impl AnthropicHttpClient {
    pub(crate) fn new(bearer_token: String) -> Self {
        Self {
            inner: reqwest::Client::new(),
            bearer_token: Some(bearer_token),
        }
    }

    fn normalized_request<T>(&self, req: Request<T>) -> http_client::Result<Request<Bytes>>
    where
        T: Into<Bytes>,
    {
        let (mut parts, body) = req.into_parts();
        parts.headers.remove("x-api-key");
        if let Some(token) = self.bearer_token.as_deref()
            && let Ok(value) = http::HeaderValue::from_str(&format!("Bearer {token}"))
        {
            parts.headers.insert(http::header::AUTHORIZATION, value);
        }
        parts.headers.insert(
            http::HeaderName::from_static("anthropic-beta"),
            http::HeaderValue::from_static(ANTHROPIC_OAUTH_BETA),
        );
        parts.headers.insert(
            http::HeaderName::from_static("anthropic-dangerous-direct-browser-access"),
            http::HeaderValue::from_static("true"),
        );
        parts.headers.insert(
            http::HeaderName::from_static("x-app"),
            http::HeaderValue::from_static("cli"),
        );

        let body = body.into();
        let body = if is_messages_path(parts.uri.path()) {
            prepend_claude_code_system(body)
        } else {
            body
        };

        let mut builder = Request::builder()
            .method(parts.method)
            .uri(parts.uri)
            .version(parts.version);
        if let Some(headers) = builder.headers_mut() {
            *headers = parts.headers;
        }
        builder.body(body).map_err(http_client::Error::Protocol)
    }
}

impl HttpClientExt for AnthropicHttpClient {
    fn send<T, U>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes>,
        T: Send,
        U: From<Bytes>,
        U: Send + 'static,
    {
        let inner = self.inner.clone();
        let req = self.normalized_request(req);
        async move {
            let req = req?;
            inner.send(req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        self.inner.send_multipart(req)
    }

    fn send_streaming<T>(
        &self,
        req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let inner = self.inner.clone();
        let req = self.normalized_request(req);
        async move {
            let req = req?;
            inner.send_streaming(req).await
        }
    }
}

fn is_messages_path(path: &str) -> bool {
    path.ends_with("/messages")
}

fn prepend_claude_code_system(body: Bytes) -> Bytes {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    let Some(obj) = value.as_object_mut() else {
        return body;
    };

    let claude_block = serde_json::json!({
        "type": "text",
        "text": CLAUDE_CODE_SYSTEM_PROMPT,
    });

    match obj.get_mut("system") {
        // Already an array of content blocks: prepend unless it's already first.
        Some(serde_json::Value::Array(items)) => {
            if first_system_block_is_claude_code(items) {
                return body;
            }
            items.insert(0, claude_block);
        }
        // A bare string system prompt: lift it into the array form behind the
        // required Claude Code block.
        Some(serde_json::Value::String(text)) => {
            let existing = std::mem::take(text);
            obj.insert(
                "system".to_string(),
                serde_json::json!([
                    claude_block,
                    { "type": "text", "text": existing },
                ]),
            );
        }
        // No system prompt at all.
        _ => {
            obj.insert("system".to_string(), serde_json::json!([claude_block]));
        }
    }

    serde_json::to_vec(&value).map(Bytes::from).unwrap_or(body)
}

fn first_system_block_is_claude_code(items: &[serde_json::Value]) -> bool {
    items
        .first()
        .and_then(|item| item.get("text"))
        .and_then(serde_json::Value::as_str)
        == Some(CLAUDE_CODE_SYSTEM_PROMPT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepends_claude_code_block_to_system_array() {
        let body = Bytes::from(
            serde_json::json!({
                "system": [{ "type": "text", "text": "Real prompt." }],
                "messages": []
            })
            .to_string(),
        );

        let value: serde_json::Value =
            serde_json::from_slice(&prepend_claude_code_system(body)).unwrap();

        let system = value["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(system[1]["text"], "Real prompt.");
    }

    #[test]
    fn lifts_string_system_into_array_behind_claude_code_block() {
        let body = Bytes::from(
            serde_json::json!({ "system": "Real prompt.", "messages": [] }).to_string(),
        );

        let value: serde_json::Value =
            serde_json::from_slice(&prepend_claude_code_system(body)).unwrap();

        let system = value["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert_eq!(system[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(system[1]["text"], "Real prompt.");
    }

    #[test]
    fn adds_system_when_absent() {
        let body = Bytes::from(serde_json::json!({ "messages": [] }).to_string());

        let value: serde_json::Value =
            serde_json::from_slice(&prepend_claude_code_system(body)).unwrap();

        let system = value["system"].as_array().unwrap();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
    }

    #[test]
    fn does_not_double_prepend_claude_code_block() {
        let body = Bytes::from(
            serde_json::json!({
                "system": [
                    { "type": "text", "text": CLAUDE_CODE_SYSTEM_PROMPT },
                    { "type": "text", "text": "Real prompt." }
                ]
            })
            .to_string(),
        );

        let value: serde_json::Value =
            serde_json::from_slice(&prepend_claude_code_system(body)).unwrap();

        let system = value["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
    }
}
