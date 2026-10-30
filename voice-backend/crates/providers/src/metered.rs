//! Counts what every streamed LLM request costs, including the ones nobody
//! reads to the end.
//!
//! The provider reports token usage in the last chunk of a stream. A call
//! drops streams early all the time: the caller interrupts (barge-in), an
//! eager reply is thrown away, a hedged duplicate loses the race, a
//! guardrail stops the reply, or the first token is too slow. The provider
//! still bills those requests, but their usage chunk never arrives. This
//! wrapper sits between the pipeline and the real provider, forwards every
//! event unchanged, and when a stream ends records its real usage, or an
//! estimate from the request and the output seen so far when the usage
//! never came. Non-streaming `complete` calls are passed through; their
//! callers bill `Completion::usage` themselves.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::Duration,
};

use async_trait::async_trait;
use common::assets::TokenUsage;
use tokio::sync::mpsc;

use crate::{
    llm::{ChatRequest, Completion, LlmEndpoint, LlmEvent, LlmProvider, LlmStream},
    ProviderError,
};

/// Bytes per token when no real usage has been seen yet for a model.
/// English is about 4; Indian scripts take 3 bytes a character but fewer
/// characters a token, which lands near the same number.
const DEFAULT_BYTES_PER_TOKEN: f64 = 4.0;
/// After the reader has seen `Finish`, how long to keep reading for the
/// usage chunk before giving up and estimating.
const USAGE_GRACE: Duration = Duration::from_secs(2);

/// Token usage of every stream, split into reported and estimated.
#[derive(Debug, Clone, Default)]
pub struct MeterReport {
    /// Usage the provider reported, per model.
    pub reported: HashMap<String, TokenUsage>,
    /// Estimated usage of streams that ended before the provider reported
    /// it, per model.
    pub estimated: HashMap<String, TokenUsage>,
    /// Streams whose usage was estimated.
    pub estimated_streams: u64,
}

impl MeterReport {
    /// Reported and estimated usage together, per model.
    pub fn total(&self) -> HashMap<String, TokenUsage> {
        let mut out = self.reported.clone();
        for (model, u) in &self.estimated {
            out.entry(model.clone()).or_default().add(*u);
        }
        out
    }
}

#[derive(Default)]
struct Ledger {
    report: MeterReport,
    /// Prompt bytes per prompt token, learnt from reported usage.
    bytes_per_token: HashMap<String, f64>,
}

impl Ledger {
    fn commit(
        &mut self,
        model: &str,
        prompt_bytes: usize,
        output_bytes: usize,
        reported: Option<TokenUsage>,
    ) {
        if let Some(u) = reported {
            if u.prompt_tokens > 0 {
                self.bytes_per_token.insert(
                    model.to_owned(),
                    (prompt_bytes as f64 / u.prompt_tokens as f64).clamp(1.0, 8.0),
                );
            }
            self.report
                .reported
                .entry(model.to_owned())
                .or_default()
                .add(u);
            return;
        }
        let per = self
            .bytes_per_token
            .get(model)
            .copied()
            .unwrap_or(DEFAULT_BYTES_PER_TOKEN);
        let tokens = |bytes: usize| (bytes as f64 / per).ceil() as u64;
        let estimate = TokenUsage {
            prompt_tokens: tokens(prompt_bytes),
            // Billed at the full input rate: no way to know what was cached.
            cached_prompt_tokens: 0,
            completion_tokens: tokens(output_bytes),
        };
        self.report.estimated_streams += 1;
        self.report
            .estimated
            .entry(model.to_owned())
            .or_default()
            .add(estimate);
        metrics::counter!("llm_estimated_streams_total", "model" => model.to_owned()).increment(1);
    }
}

/// An [`LlmProvider`] that records the usage of every stream it opens.
/// Make one per call and read [`MeteredLlm::report`] when the call ends.
pub struct MeteredLlm {
    inner: Arc<dyn LlmProvider>,
    ledger: Arc<Mutex<Ledger>>,
    /// Streams whose usage is not committed yet.
    open: Arc<AtomicUsize>,
}

impl MeteredLlm {
    /// Wraps `inner`.
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self {
            inner,
            ledger: Arc::default(),
            open: Arc::default(),
        }
    }

    /// Waits (up to `max`) until every stream opened so far has recorded
    /// its usage. Streams commit within moments of being dropped, or up to
    /// `USAGE_GRACE` when the usage chunk is late.
    pub async fn settle(&self, max: Duration) {
        let deadline = tokio::time::Instant::now() + max;
        while self.open.load(Ordering::Acquire) > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Usage so far. Streams still open are not included yet.
    pub fn report(&self) -> MeterReport {
        self.ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .report
            .clone()
    }
}

/// Size of what is sent to the model: messages, tool calls and tool specs.
fn request_bytes(req: &ChatRequest) -> usize {
    let messages: usize = req
        .messages
        .iter()
        .map(|m| {
            m.content.as_deref().map_or(0, str::len)
                + m.tool_calls
                    .iter()
                    .map(|c| c.function.name.len() + c.function.arguments.len())
                    .sum::<usize>()
                + 8
        })
        .sum();
    let tools: usize = req
        .tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.parameters.to_string().len())
        .sum();
    messages + tools
}

fn output_bytes(event: &LlmEvent) -> usize {
    match event {
        LlmEvent::Token(t) => t.len(),
        LlmEvent::ToolCalls(calls) => calls
            .iter()
            .map(|c| c.function.name.len() + c.function.arguments.len())
            .sum(),
        _ => 0,
    }
}

#[async_trait]
impl LlmProvider for MeteredLlm {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    async fn stream(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<LlmStream, ProviderError> {
        let prompt_bytes = request_bytes(request);
        // A request that fails before streaming is not billed.
        let mut inner = self.inner.stream(endpoint, request).await?;
        let (tx, rx) = mpsc::channel(64);
        let ledger = self.ledger.clone();
        let open = self.open.clone();
        open.fetch_add(1, Ordering::AcqRel);
        let model = endpoint.model.clone();
        tokio::spawn(async move {
            let mut reported: Option<TokenUsage> = None;
            let mut out = 0usize;
            let mut finished = false;
            loop {
                let event = tokio::select! {
                    event = inner.recv() => event,
                    () = tx.closed() => break,
                };
                let Some(event) = event else { break };
                if let Ok(e) = &event {
                    out += output_bytes(e);
                    match e {
                        LlmEvent::Usage(u) => {
                            reported.get_or_insert_with(TokenUsage::default).add(*u)
                        }
                        LlmEvent::Finish { .. } => finished = true,
                        _ => {}
                    }
                }
                if tx.send(event).await.is_err() {
                    break;
                }
            }
            // The reader stopped after the reply was complete: the usage
            // chunk may still be on its way, so wait for it briefly. A reply
            // cut short is dropped at once, which cancels the request.
            if finished && reported.is_none() {
                let _ = tokio::time::timeout(USAGE_GRACE, async {
                    while let Some(event) = inner.recv().await {
                        if let Ok(LlmEvent::Usage(u)) = event {
                            reported.get_or_insert_with(TokenUsage::default).add(u);
                        }
                    }
                })
                .await;
            }
            drop(inner);
            ledger
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .commit(&model, prompt_bytes, out, reported);
            open.fetch_sub(1, Ordering::AcqRel);
        });
        Ok(rx)
    }

    async fn complete(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<Completion, ProviderError> {
        self.inner.complete(endpoint, request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, Role};

    /// Streams scripted events, then stays open (like a slow provider).
    /// Each stream takes the next script; the last one repeats.
    struct Scripted(Vec<Vec<LlmEvent>>, AtomicUsize);

    fn scripted(scripts: Vec<Vec<LlmEvent>>) -> Arc<Scripted> {
        Arc::new(Scripted(scripts, AtomicUsize::new(0)))
    }

    #[async_trait]
    impl LlmProvider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn stream(
            &self,
            _: &LlmEndpoint,
            _: &ChatRequest,
        ) -> Result<LlmStream, ProviderError> {
            let i = self.1.fetch_add(1, Ordering::AcqRel).min(self.0.len() - 1);
            let (tx, rx) = mpsc::channel(16);
            let events = self.0[i].clone();
            tokio::spawn(async move {
                for e in events {
                    if tx.send(Ok(e)).await.is_err() {
                        return;
                    }
                }
                tx.closed().await;
            });
            Ok(rx)
        }
        async fn complete(
            &self,
            _: &LlmEndpoint,
            _: &ChatRequest,
        ) -> Result<Completion, ProviderError> {
            Ok(Completion::default())
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            messages: vec![ChatMessage::text(Role::User, "x".repeat(4000))],
            tools: Vec::new(),
            max_tokens: 100,
            temperature: 0.0,
            disable_reasoning: true,
            json_schema: None,
        }
    }

    fn endpoint() -> LlmEndpoint {
        LlmEndpoint {
            url: "http://test".into(),
            model: "m".into(),
        }
    }

    #[tokio::test]
    async fn reported_usage_is_kept() {
        let usage = TokenUsage {
            prompt_tokens: 1000,
            cached_prompt_tokens: 0,
            completion_tokens: 5,
        };
        let llm = MeteredLlm::new(scripted(vec![vec![
            LlmEvent::Token("Hello.".into()),
            LlmEvent::Usage(usage),
            LlmEvent::Finish { reason: None },
        ]]));
        let mut rx = llm.stream(&endpoint(), &request()).await.expect("stream");
        while let Some(Ok(e)) = rx.recv().await {
            if matches!(e, LlmEvent::Finish { .. }) {
                break;
            }
        }
        drop(rx);
        llm.settle(Duration::from_secs(2)).await;
        let r = llm.report();
        assert_eq!(r.reported["m"].prompt_tokens, 1000);
        assert_eq!(r.estimated_streams, 0);
    }

    #[tokio::test]
    async fn a_stream_dropped_early_is_estimated() {
        let llm = MeteredLlm::new(scripted(vec![vec![LlmEvent::Token(
            "Sure, the plot".into(),
        )]]));
        let mut rx = llm.stream(&endpoint(), &request()).await.expect("stream");
        let _ = rx.recv().await;
        drop(rx); // barge-in
        llm.settle(Duration::from_secs(1)).await;
        let r = llm.report();
        assert_eq!(r.estimated_streams, 1);
        let u = r.estimated["m"];
        assert!(u.prompt_tokens >= 1000, "{u:?}");
        assert!(u.completion_tokens >= 3, "{u:?}");
        assert_eq!(r.total()["m"].prompt_tokens, u.prompt_tokens);
    }

    #[tokio::test]
    async fn estimates_learn_the_bytes_per_token_ratio() {
        let usage = TokenUsage {
            prompt_tokens: 2000, // 2 bytes a token for this model
            cached_prompt_tokens: 0,
            completion_tokens: 1,
        };
        // The second stream never reports usage (a hedged copy that loses).
        let llm = MeteredLlm::new(scripted(vec![
            vec![LlmEvent::Usage(usage), LlmEvent::Finish { reason: None }],
            vec![],
        ]));
        let mut rx = llm.stream(&endpoint(), &request()).await.expect("stream");
        while let Some(Ok(e)) = rx.recv().await {
            if matches!(e, LlmEvent::Finish { .. }) {
                break;
            }
        }
        drop(rx);
        llm.settle(Duration::from_secs(2)).await;
        let unread = llm.stream(&endpoint(), &request()).await.expect("stream");
        drop(unread);
        llm.settle(Duration::from_secs(2)).await;
        let r = llm.report();
        assert_eq!(r.estimated["m"].prompt_tokens, 2000);
    }
}
