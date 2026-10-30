//! `voice-load`: many simulated calls at once, for capacity tests.
//!
//! Starts `--calls` calls through `POST /v1/dev/sim-calls` (spread over
//! `--spread-secs`), and drives each one like Plivo does: 20 ms μ-law frames
//! in real time, the `--wav` files as caller turns, `playedStream` and
//! `clearedAudio` replies. For every call it measures the time from the end
//! of the caller's speech to the agent's first audio, and how long the
//! caller's playback buffer ran dry in the middle of agent speech (heard as
//! stutter). The last line printed is `RESULT {json}` with the summary.
//!
//! Meant for a server with `PROVIDER_MODE=fake` in an offline container
//! (`scripts/stress.sh`); `--check-offline` exits with an error when the
//! internet (Sarvam, Plivo) is reachable from here.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser, Debug)]
#[command(about = "Run many simulated calls at once against voice-server")]
struct Args {
    /// Server base URL.
    #[arg(long, default_value = "http://localhost:8080")]
    server: String,
    /// API key (X-API-Key).
    #[arg(long, env = "API_KEY", default_value = "change-me")]
    api_key: String,
    /// Caller turns, in order (16-bit PCM WAV).
    #[arg(long = "wav")]
    wavs: Vec<PathBuf>,
    /// Calls to run at the same time.
    #[arg(long, default_value_t = 1)]
    calls: usize,
    /// Calls start evenly over this many seconds.
    #[arg(long, default_value_t = 5)]
    spread_secs: u64,
    /// Max wait for the agent to answer a turn.
    #[arg(long, default_value_t = 15)]
    turn_timeout_secs: u64,
    /// Only check that Sarvam, Plivo and the internet are unreachable.
    #[arg(long)]
    check_offline: bool,
}

const FRAME: usize = 160;
const FRAME_MS: u64 = 20;
/// A dry playback buffer shorter than this is not audible.
const STUTTER_MIN: Duration = Duration::from_millis(40);
/// Longer silences are pauses between turns, not stutter.
const STUTTER_MAX: Duration = Duration::from_millis(1500);

/// What the agent sent on one call.
#[derive(Default)]
struct AgentAudio {
    last_frame: Option<Instant>,
    frame_times: Vec<Instant>,
    stutter_ms: u64,
    stutter_events: u32,
}

/// Measurements of one call.
#[derive(Debug, Default)]
struct CallResult {
    error: Option<String>,
    create_ms: u64,
    greeting_ms: Option<u64>,
    replies: Vec<Option<u64>>,
    stutter_ms: u64,
    stutter_events: u32,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    if args.check_offline {
        return check_offline().await;
    }
    if args.wavs.is_empty() {
        bail!("give at least one --wav caller turn");
    }
    let turns: Arc<Vec<(Vec<u8>, usize)>> = Arc::new(
        args.wavs
            .iter()
            .map(|p| load_turn(p).with_context(|| format!("reading {}", p.display())))
            .collect::<Result<_, _>>()?,
    );
    let args = Arc::new(args);
    let http = reqwest::Client::new();
    wait_ready(&http, &args.server).await?;
    let started = Instant::now();
    let gap = Duration::from_millis(args.spread_secs * 1000 / args.calls.max(1) as u64);
    let mut tasks = Vec::with_capacity(args.calls);
    for i in 0..args.calls {
        let (args, http, turns) = (args.clone(), http.clone(), turns.clone());
        tasks.push(tokio::spawn(async move {
            tokio::time::sleep(gap * i as u32).await;
            let mut result = CallResult::default();
            if let Err(e) = run_call(&args, &http, &turns, &mut result).await {
                result.error = Some(format!("{e:#}"));
            }
            result
        }));
    }
    let mut results = Vec::with_capacity(tasks.len());
    for t in tasks {
        results.push(t.await.unwrap_or_else(|e| CallResult {
            error: Some(format!("task failed: {e}")),
            ..CallResult::default()
        }));
    }
    let summary = summarize(&results, turns.len(), started.elapsed());
    println!("RESULT {summary}");
    Ok(())
}

/// Drives one simulated call from creation to hang-up.
async fn run_call(
    args: &Args,
    http: &reqwest::Client,
    turns: &[(Vec<u8>, usize)],
    result: &mut CallResult,
) -> anyhow::Result<()> {
    let server = args.server.trim_end_matches('/');
    let created = Instant::now();
    let resp = http
        .post(format!("{server}/v1/dev/sim-calls"))
        .header("x-api-key", &args.api_key)
        .timeout(Duration::from_secs(30))
        .json(&json!({
            "to": "",
            "lead_name": "Load Test",
            "system_prompt": "You are a friendly sales assistant. Qualify the lead.",
            "language": "en-IN",
        }))
        .send()
        .await
        .context("creating the call")?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("create: {status} {}", body["error"]);
    }
    result.create_ms = created.elapsed().as_millis() as u64;
    let call_id = body["call_id"].as_str().unwrap_or_default().to_owned();
    let stream_url = body["stream_url"].as_str().unwrap_or_default().to_owned();

    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(stream_url.as_str()),
    )
    .await
    .context("media WebSocket connect timed out")?
    .context("connecting the media WebSocket")?;
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(1024);
    let writer = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    out_tx
        .send(
            json!({
                "event": "start", "sequenceNumber": 1, "extra_headers": "",
                "start": { "callId": call_id, "streamId": "load-stream", "accountId": "load",
                           "tracks": ["inbound"], "mediaFormat": { "encoding": "audio/x-mulaw", "sampleRate": 8000 } }
            })
            .to_string(),
        )
        .await?;
    let stream_started = Instant::now();

    // Receiver: track the caller's playback buffer; answer checkpoints when played.
    let agent = Arc::new(Mutex::new(AgentAudio::default()));
    let agent_rx = agent.clone();
    let replies = out_tx.clone();
    let reader = tokio::spawn(async move {
        let mut play_end = Instant::now();
        let mut seq = 1000u64;
        while let Some(Ok(msg)) = stream.next().await {
            let Message::Text(text) = msg else {
                if matches!(msg, Message::Close(_)) {
                    break;
                }
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            seq += 1;
            match v["event"].as_str() {
                Some("playAudio") => {
                    let len = STANDARD
                        .decode(v["media"]["payload"].as_str().unwrap_or_default())
                        .map_or(0, |b| b.len());
                    let now = Instant::now();
                    let mut a = lock(&agent_rx);
                    if a.last_frame.is_some() && now > play_end {
                        let dry = now - play_end;
                        if (STUTTER_MIN..STUTTER_MAX).contains(&dry) {
                            a.stutter_ms += dry.as_millis() as u64;
                            a.stutter_events += 1;
                        }
                    }
                    play_end = play_end.max(now) + Duration::from_millis(len as u64 * 1000 / 8000);
                    a.last_frame = Some(now);
                    a.frame_times.push(now);
                }
                Some("checkpoint") => {
                    let name = v["name"].as_str().unwrap_or_default().to_owned();
                    let (at, replies) = (play_end, replies.clone());
                    tokio::spawn(async move {
                        tokio::time::sleep_until(at.into()).await;
                        let _ = replies
                            .send(json!({"event": "playedStream", "sequenceNumber": seq, "streamId": "load-stream", "name": name}).to_string())
                            .await;
                    });
                }
                Some("clearAudio") => {
                    play_end = Instant::now();
                    let _ = replies
                        .send(json!({"event": "clearedAudio", "sequenceNumber": seq, "streamId": "load-stream"}).to_string())
                        .await;
                }
                _ => {}
            }
        }
    });

    // Sender: a frame every 20 ms, silence unless a turn is playing.
    let (turn_tx, mut turn_rx) = mpsc::channel::<(Vec<u8>, oneshot::Sender<Instant>)>(1);
    let frames_tx = out_tx.clone();
    let sender = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(FRAME_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        let silence = vec![audio::mulaw::SILENCE; FRAME];
        let mut current: Option<(Vec<u8>, usize, oneshot::Sender<Instant>)> = None;
        let mut chunk = 0u64;
        loop {
            tick.tick().await;
            if current.is_none() {
                if let Ok((audio, done)) = turn_rx.try_recv() {
                    current = Some((audio, 0, done));
                }
            }
            let frame = match &mut current {
                Some((audio, pos, _)) if *pos < audio.len() => {
                    let end = (*pos + FRAME).min(audio.len());
                    let mut f = audio[*pos..end].to_vec();
                    f.resize(FRAME, audio::mulaw::SILENCE);
                    *pos = end;
                    f
                }
                _ => silence.clone(),
            };
            if current.as_ref().is_some_and(|(a, pos, _)| *pos >= a.len()) {
                if let Some((_, _, done)) = current.take() {
                    let _ = done.send(Instant::now());
                }
            }
            chunk += 1;
            let msg = json!({
                "event": "media", "sequenceNumber": chunk + 1, "streamId": "load-stream",
                "media": { "track": "inbound", "timestamp": (chunk * FRAME_MS).to_string(), "chunk": chunk,
                           "payload": STANDARD.encode(&frame) }
            });
            if frames_tx.send(msg.to_string()).await.is_err() {
                break;
            }
        }
    });

    result.greeting_ms = first_audio_after(&agent, stream_started, Duration::from_secs(15)).await;
    wait_idle(&agent, Duration::from_millis(900), Duration::from_secs(30)).await;
    let timeout = Duration::from_secs(args.turn_timeout_secs);
    for (mulaw, voiced_end) in turns {
        let (done_tx, done_rx) = oneshot::channel();
        turn_tx.send((mulaw.clone(), done_tx)).await?;
        let finished = done_rx.await.unwrap_or_else(|_| Instant::now());
        let trailing = Duration::from_millis(((mulaw.len() - voiced_end) as u64 * 1000) / 8000);
        let speech_end = finished.checked_sub(trailing).unwrap_or(finished);
        result
            .replies
            .push(first_audio_after(&agent, speech_end, timeout).await);
        wait_idle(&agent, Duration::from_millis(1000), Duration::from_secs(30)).await;
    }

    sender.abort();
    drop(out_tx);
    drop(turn_tx);
    tokio::time::sleep(Duration::from_millis(300)).await;
    reader.abort();
    writer.abort();
    let a = lock(&agent);
    result.stutter_ms = a.stutter_ms;
    result.stutter_events = a.stutter_events;
    Ok(())
}

/// Milliseconds from `since` to the first agent frame after it.
async fn first_audio_after(
    agent: &Arc<Mutex<AgentAudio>>,
    since: Instant,
    timeout: Duration,
) -> Option<u64> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(t) = lock(agent).frame_times.iter().find(|t| **t >= since) {
            return Some(t.duration_since(since).as_millis() as u64);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    None
}

/// Waits until the agent has sent nothing for `quiet`.
async fn wait_idle(agent: &Arc<Mutex<AgentAudio>>, quiet: Duration, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if lock(agent).last_frame.is_none_or(|t| t.elapsed() >= quiet) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// p50 / p95 / max of a list of milliseconds.
fn spread(mut v: Vec<u64>) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    v.sort_unstable();
    let pick = |p: f64| v[((p / 100.0 * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1];
    json!({ "p50": pick(50.0), "p95": pick(95.0), "max": v[v.len() - 1] })
}

fn summarize(results: &[CallResult], turns: usize, took: Duration) -> Value {
    let ok: Vec<&CallResult> = results.iter().filter(|r| r.error.is_none()).collect();
    let mut errors: Vec<&str> = results.iter().filter_map(|r| r.error.as_deref()).collect();
    errors.sort_unstable();
    errors.dedup();
    errors.truncate(5);
    json!({
        "calls": results.len(),
        "completed": ok.len(),
        "failed": results.len() - ok.len(),
        "turns_per_call": turns,
        "missed_replies": ok.iter().map(|r| r.replies.iter().filter(|x| x.is_none()).count()).sum::<usize>()
            + ok.iter().filter(|r| r.greeting_ms.is_none()).count(),
        "create_ms": spread(ok.iter().map(|r| r.create_ms).collect()),
        "greeting_ms": spread(ok.iter().filter_map(|r| r.greeting_ms).collect()),
        "reply_ms": spread(ok.iter().flat_map(|r| r.replies.iter().flatten().copied()).collect()),
        "stutter_ms_per_call": spread(ok.iter().map(|r| r.stutter_ms).collect()),
        "stutter_events": ok.iter().map(|r| u64::from(r.stutter_events)).sum::<u64>(),
        "errors": errors,
        "seconds": took.as_secs(),
    })
}

/// Waits up to two minutes for `/readyz` (the server loads its models first).
async fn wait_ready(http: &reqwest::Client, server: &str) -> anyhow::Result<()> {
    let url = format!("{}/readyz", server.trim_end_matches('/'));
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        let ok = http
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if ok {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("{url} not ready after 120 s")
}

/// Fails when any of Sarvam, Plivo or a public IP answers from here.
async fn check_offline() -> anyhow::Result<()> {
    let mut reachable = Vec::new();
    for target in [
        "api.sarvam.ai:443",
        "api.plivo.com:443",
        "1.1.1.1:443",
        "8.8.8.8:53",
    ] {
        let connect = tokio::net::TcpStream::connect(target);
        if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(4), connect).await {
            reachable.push(target);
        }
    }
    if reachable.is_empty() {
        println!("offline: Sarvam, Plivo and the internet are unreachable");
        Ok(())
    } else {
        bail!("NOT offline, reachable: {}", reachable.join(", "))
    }
}

/// Reads a WAV, converts to 8 kHz mono μ-law, and finds the last voiced sample.
fn load_turn(path: &PathBuf) -> anyhow::Result<(Vec<u8>, usize)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        bail!("expected 16-bit PCM WAV");
    }
    let raw: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>()?;
    let channels = usize::from(spec.channels.max(1));
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|c| c.iter().map(|&s| f32::from(s)).sum::<f32>() / channels as f32)
        .collect();
    let ratio = f64::from(spec.sample_rate) / 8000.0;
    let out_len = (mono.len() as f64 / ratio) as usize;
    let resampled: Vec<i16> = (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let a = mono.get(j).copied().unwrap_or(0.0);
            let b = mono.get(j + 1).copied().unwrap_or(a);
            (a + (b - a) * frac).clamp(-32768.0, 32767.0) as i16
        })
        .collect();
    let voiced_end = resampled
        .iter()
        .rposition(|s| s.unsigned_abs() > 1000)
        .map_or(resampled.len(), |i| i + 1);
    let mulaw = resampled.iter().map(|&s| audio::mulaw::encode(s)).collect();
    Ok((mulaw, voiced_end))
}
