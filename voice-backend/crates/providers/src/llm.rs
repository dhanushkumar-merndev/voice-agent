//! Chat-completion abstraction (OpenAI-compatible shapes).

use async_trait::async_trait;
use common::assets::TokenUsage;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::ProviderError;

/// Message author.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System instructions.
    System,
    /// The caller.
    User,
    /// The agent.
    Assistant,
    /// A tool result.
    Tool,
}

/// One chat message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Author.
    pub role: Role,
    /// Text content (absent on pure tool-call messages).
    #[serde(default)]
    pub content: Option<String>,
    /// Tool calls requested by the assistant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For `role = tool`: the call this result answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// A message with text content.
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A tool result message.
    pub fn tool_result(call_id: &str, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.to_owned()),
        }
    }
}

/// A function call requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Call ID, echoed in the tool result.
    pub id: String,
    /// Always `function`.
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    /// Function name and JSON arguments.
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".to_owned()
}

/// Function name and arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Function name.
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

/// A tool the model may call.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Function name.
    pub name: &'static str,
    /// What it does (shown to the model).
    pub description: &'static str,
    /// JSON Schema of the arguments.
    pub parameters: Value,
}

/// Where to send a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmEndpoint {
    /// Full chat-completions URL.
    pub url: String,
    /// Model ID.
    pub model: String,
}

/// A chat-completion request.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// Conversation, system message first.
    pub messages: Vec<ChatMessage>,
    /// Tools offered to the model (empty: none).
    pub tools: Vec<ToolSpec>,
    /// Output token cap.
    pub max_tokens: u32,
    /// Sampling temperature.
    pub temperature: f32,
    /// Send `reasoning_effort: null` to disable thinking (Sarvam models).
    pub disable_reasoning: bool,
    /// Structured output: `(schema name, JSON Schema)`.
    pub json_schema: Option<(String, Value)>,
}

/// Streaming events.
#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    /// Visible text delta.
    Token(String),
    /// The model requested tool calls (complete, emitted once at the end).
    ToolCalls(Vec<ToolCall>),
    /// Token usage for the request.
    Usage(TokenUsage),
    /// The response is complete.
    Finish {
        /// `stop`, `length`, `tool_calls`, ...
        reason: Option<String>,
    },
}

/// Stream of events. Dropping the receiver cancels the request.
pub type LlmStream = mpsc::Receiver<Result<LlmEvent, ProviderError>>;

/// A complete (non-streaming) response.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    /// Text content.
    pub content: String,
    /// Tool calls.
    pub tool_calls: Vec<ToolCall>,
    /// Token usage.
    pub usage: TokenUsage,
    /// Finish reason.
    pub finish_reason: Option<String>,
}

/// A chat-completion provider.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Short provider name for metrics and logs.
    fn name(&self) -> &'static str;

    /// Starts a streaming request. Returns once response headers arrive
    /// (HTTP errors are returned here, not on the stream).
    async fn stream(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<LlmStream, ProviderError>;

    /// Runs a non-streaming request.
    async fn complete(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<Completion, ProviderError>;
}
