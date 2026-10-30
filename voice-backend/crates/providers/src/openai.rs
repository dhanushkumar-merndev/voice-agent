//! OpenAI-compatible chat completions client (Sarvam by default).
//!
//! `POST {url}` with JSON `{model, messages, stream, max_tokens,
//! temperature, tools?, response_format?, reasoning_effort?}`. Streaming
//! responses are server-sent events: `data: {chunk}` lines ending with
//! `data: [DONE]`. Each chunk carries `choices[0].delta.content`,
//! `choices[0].delta.tool_calls[]` (fragments keyed by `index`),
//! `choices[0].finish_reason`, and optionally `usage`.
//!
//! Sarvam accepts both `Authorization: Bearer` and `api-subscription-key`;
//! both are sent. `reasoning_effort: null` disables thinking on
//! `sarvam-105b*`, which matters on live calls (reasoning tokens would eat
//! the small `max_tokens` budget and add latency). Reasoning deltas that
//! still arrive are dropped and counted.

use std::collections::BTreeMap;

use async_trait::async_trait;
use common::assets::TokenUsage;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::{
    llm::{
        ChatRequest, Completion, FunctionCall, LlmEndpoint, LlmEvent, LlmProvider, LlmStream,
        ToolCall,
    },
    ProviderError,
};

/// OpenAI-compatible provider.
#[derive(Debug, Clone)]
pub struct OpenAiCompatLlm {
    http: reqwest::Client,
    api_key: String,
}

impl OpenAiCompatLlm {
    /// Creates the provider with a shared (pooled, keep-alive) HTTP client.
    pub fn new(http: reqwest::Client, api_key: &str) -> Self {
        Self {
            http,
            api_key: api_key.to_owned(),
        }
    }

    async fn send(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
        stream: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let body = request_body(endpoint, request, stream);
        let resp = self
            .http
            .post(&endpoint.url)
            .bearer_auth(&self.api_key)
            .header("api-subscription-key", &self.api_key)
            .json(&body)
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let body: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect();
        record_error(&status.to_string());
        Err(ProviderError::Status { status, body })
    }
}

fn record_error(code: &str) {
    metrics::counter!(
        common::metric_names::SARVAM_ERRORS_TOTAL,
        "service" => "llm",
        "code" => code.to_owned()
    )
    .increment(1);
}

/// Builds the JSON request body.
pub fn request_body(endpoint: &LlmEndpoint, request: &ChatRequest, stream: bool) -> Value {
    let mut body = json!({
        "model": endpoint.model,
        "messages": request.messages,
        "stream": stream,
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
    });
    if stream {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if request.disable_reasoning {
        body["reasoning_effort"] = Value::Null;
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body["tool_choice"] = json!("auto");
    }
    if let Some((name, schema)) = &request.json_schema {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": { "name": name, "schema": schema, "strict": true }
        });
    }
    body
}

fn parse_usage(v: &Value) -> Option<TokenUsage> {
    let u = v.get("usage")?;
    if u.is_null() {
        return None;
    }
    Some(TokenUsage {
        prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
        cached_prompt_tokens: u["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0),
        completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
    })
}

/// Accumulates streamed tool-call fragments by `index`.
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    calls: BTreeMap<u64, (String, String, String)>,
}

impl ToolCallAccumulator {
    /// Adds the `delta.tool_calls` array of one chunk.
    pub fn push(&mut self, deltas: &Value) {
        let Some(items) = deltas.as_array() else {
            return;
        };
        for (pos, item) in items.iter().enumerate() {
            let index = item["index"].as_u64().unwrap_or(pos as u64);
            let entry = self.calls.entry(index).or_default();
            if let Some(id) = item["id"].as_str() {
                entry.0.push_str(id);
            }
            if let Some(name) = item["function"]["name"].as_str() {
                entry.1.push_str(name);
            }
            if let Some(args) = item["function"]["arguments"].as_str() {
                entry.2.push_str(args);
            }
        }
    }

    /// `true` if no fragments were seen.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// The assembled calls.
    pub fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .map(|(index, (id, name, arguments))| ToolCall {
                id: if id.is_empty() {
                    format!("call_{index}")
                } else {
                    id
                },
                kind: "function".to_owned(),
                function: FunctionCall {
                    name,
                    arguments: if arguments.is_empty() {
                        "{}".to_owned()
                    } else {
                        arguments
                    },
                },
            })
            .collect()
    }
}

/// Parses one SSE chunk. Returns events plus whether the stream finished.
fn handle_chunk(
    v: &Value,
    tools: &mut ToolCallAccumulator,
    events: &mut Vec<LlmEvent>,
) -> Option<String> {
    let choice = &v["choices"][0];
    let delta = &choice["delta"];
    if let Some(text) = delta["content"].as_str() {
        if !text.is_empty() {
            events.push(LlmEvent::Token(text.to_owned()));
        }
    }
    if delta["reasoning_content"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        metrics::counter!("llm_reasoning_chunks_total").increment(1);
    }
    if delta.get("tool_calls").is_some() {
        tools.push(&delta["tool_calls"]);
    }
    if let Some(usage) = parse_usage(v) {
        events.push(LlmEvent::Usage(usage));
    }
    choice["finish_reason"].as_str().map(str::to_owned)
}

#[async_trait]
impl LlmProvider for OpenAiCompatLlm {
    fn name(&self) -> &'static str {
        "openai-compat"
    }

    async fn stream(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<LlmStream, ProviderError> {
        let resp = self.send(endpoint, request, true).await?;
        let (tx, rx) = mpsc::channel(128);
        tokio::spawn(async move {
            let mut body = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::with_capacity(8192);
            let mut tools = ToolCallAccumulator::default();
            let mut finish: Option<String> = None;
            let mut events = Vec::new();
            let mut done = false;
            while !done {
                let chunk = tokio::select! {
                    () = tx.closed() => return, // receiver dropped: cancel
                    chunk = body.next() => chunk,
                };
                let bytes = match chunk {
                    None => break,
                    Some(Ok(b)) => b,
                    Some(Err(e)) => {
                        let _ = tx.send(Err(ProviderError::from(e))).await;
                        return;
                    }
                };
                buf.extend_from_slice(&bytes);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line);
                    let line = line.trim();
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data == "[DONE]" {
                        done = true;
                        break;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    if let Some(err) = v.get("error") {
                        let _ = tx.send(Err(ProviderError::Protocol(err.to_string()))).await;
                        return;
                    }
                    if let Some(reason) = handle_chunk(&v, &mut tools, &mut events) {
                        finish = Some(reason);
                    }
                    for e in events.drain(..) {
                        if tx.send(Ok(e)).await.is_err() {
                            return;
                        }
                    }
                }
            }
            if !tools.is_empty() {
                let _ = tx.send(Ok(LlmEvent::ToolCalls(tools.finish()))).await;
            }
            let _ = tx.send(Ok(LlmEvent::Finish { reason: finish })).await;
        });
        Ok(rx)
    }

    async fn complete(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<Completion, ProviderError> {
        let v: Value = self.send(endpoint, request, false).await?.json().await?;
        let message = &v["choices"][0]["message"];
        let tool_calls = message["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|c| serde_json::from_value::<ToolCall>(c.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Completion {
            content: message["content"].as_str().unwrap_or_default().to_owned(),
            tool_calls,
            usage: parse_usage(&v).unwrap_or_default(),
            finish_reason: v["choices"][0]["finish_reason"].as_str().map(str::to_owned),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, Role, ToolSpec};

    fn request() -> ChatRequest {
        ChatRequest {
            messages: vec![ChatMessage::text(Role::User, "hi")],
            tools: vec![ToolSpec {
                name: "end_call",
                description: "end",
                parameters: json!({"type": "object", "properties": {}}),
            }],
            max_tokens: 80,
            temperature: 0.2,
            disable_reasoning: true,
            json_schema: None,
        }
    }

    #[test]
    fn body_has_null_reasoning_and_tools() {
        let ep = LlmEndpoint {
            url: "u".into(),
            model: "sarvam-105b-conversations".into(),
        };
        let body = request_body(&ep, &request(), true);
        assert!(body.get("reasoning_effort").is_some_and(Value::is_null));
        assert_eq!(body["max_tokens"], 80);
        assert_eq!(body["tools"][0]["function"]["name"], "end_call");
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body["messages"][0].get("tool_calls").is_none());

        let mut r = request();
        r.disable_reasoning = false;
        assert!(request_body(&ep, &r, false)
            .get("reasoning_effort")
            .is_none());
    }

    #[test]
    fn parses_stream_chunks_and_tool_fragments() {
        let mut tools = ToolCallAccumulator::default();
        let mut events = Vec::new();
        let c1 = json!({"choices":[{"delta":{"content":"Hel"}}]});
        let c2 = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"search_knowledge","arguments":"{\"que"}}]}}]});
        let c3 = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ry\":\"price\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":60}}});
        assert_eq!(handle_chunk(&c1, &mut tools, &mut events), None);
        assert_eq!(handle_chunk(&c2, &mut tools, &mut events), None);
        assert_eq!(
            handle_chunk(&c3, &mut tools, &mut events).as_deref(),
            Some("tool_calls")
        );
        assert_eq!(events[0], LlmEvent::Token("Hel".into()));
        assert_eq!(
            events[1],
            LlmEvent::Usage(TokenUsage {
                prompt_tokens: 100,
                cached_prompt_tokens: 60,
                completion_tokens: 7
            })
        );
        let calls = tools.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].function.name, "search_knowledge");
        assert_eq!(calls[0].function.arguments, r#"{"query":"price"}"#);
    }
}
