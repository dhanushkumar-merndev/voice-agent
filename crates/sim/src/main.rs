//! `voice-sim`: a stand-in for Plivo.
//!
//! Creates a simulated call (`POST /v1/dev/sim-calls`), connects to its
//! media WebSocket exactly like Plivo (start event, 20 ms μ-law `media`
//! frames in real time, `playedStream` / `clearedAudio` replies), plays
//! each `--wav` file as one caller turn and measures, per turn, the time
//! from the end of the caller's speech to the first agent audio frame.
//! The agent's audio is saved as a WAV file. Run against a local server
//! with `APP_ENV=local`; real latency numbers come from the India VPS.

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
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser, Debug)]
#[command(about = "Simulate a Plivo call against voice-server and measure latency")]
struct Args {
    /// Server base URL.
    #[arg(long, default_value = "http://localhost:8080")]
    server: String,
    /// API key (X-API-Key).
    #[arg(long, env = "API_KEY", default_value = "change-me")]
    api_key: String,
    /// Caller turns, in order (any sample rate, mono or stereo, 16-bit PCM).
    #[arg(long = "wav", required = true)]
    wavs: Vec<PathBuf>,
    /// Lead name.
    #[arg(long, default_value = "Ravi")]
    lead_name: String,
    /// Agent instructions.
    #[arg(
        long,
        default_value = "You are a friendly sales assistant for a clinic marketing agency. Qualify the lead."
    )]
    system_prompt: String,
    /// `auto` or BCP-47.
    #[arg(long, default_value = "auto")]
    language: String,
    /// Knowledge base ID.
    #[arg(long)]
    knowledge_base_id: Option<String>,
    /// Where to save the agent's audio.
    #[arg(long, default_value = "sim-out/agent.wav")]
    out: PathBuf,
    /// Max wait for the agent to answer a turn.
    #[arg(long, default_value_t = 15)]
    turn_timeout_secs: u64,
}

/// Shared view of what the agent sent.
#[derive(Default)]
struct AgentAudio {
    samples: Vec<i16>,
    frames: usize,
    last_frame: Option<Instant>,
    first_frame_after: Vec<Instant>,
}

const FRAME: usize = 160;
const FRAME_MS: u64 = 20;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    let http = reqwest::Client::new();

    let turns: Vec<(Vec<u8>, usize)> = args
        .wavs
        .iter()
        .map(|p| load_turn(p).with_context(|| format!("reading {}", p.display())))
        .collect::<Result<_, _>>()?;

    // 1. Create the simulated call.
    let mut body = json!({
        "to": "",
        "lead_name": args.lead_name,
        "system_prompt": args.system_prompt,
        "language": args.language,
    });
    if let Some(kb) = &args.knowledge_base_id {
        body["knowledge_base_id"] = json!(kb);
    }
    let created = Instant::now();
    let resp = http
        .post(format!(
            "{}/v1/dev/sim-calls",
            args.server.trim_end_matches('/')
        ))
        .header("x-api-key", &args.api_key)
        .json(&body)
        .send()
        .await
        .context("POST /v1/dev/sim-calls")?;
    let status = resp.status();
    let created_body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("server answered {status}: {created_body}");
    }
    let call_id = created_body["call_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let stream_url = created_body["stream_url"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    println!(
        "call {call_id} created in {} ms",
        created.elapsed().as_millis()
    );

    // 2. Connect like Plivo.
    let (socket, _) = tokio_tungstenite::connect_async(stream_url.as_str())
        .await
        .context("connecting to the media WebSocket")?;
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<String>(1024);
    tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let stream_id = "sim-stream";
    out_tx
        .send(
            json!({
                "event": "start", "sequenceNumber": 1, "extra_headers": "",
                "start": { "callId": call_id, "streamId": stream_id, "accountId": "sim",
                           "tracks": ["inbound"], "mediaFormat": { "encoding": "audio/x-mulaw", "sampleRate": 8000 } }
            })
            .to_string(),
        )
        .await?;
    let stream_started = Instant::now();

    // Receiver: collect agent audio; answer checkpoints when "played".
    let agent = Arc::new(Mutex::new(AgentAudio::default()));
    let agent_rx = agent.clone();
    let replies = out_tx.clone();
    let receiver = tokio::spawn(async move {
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
                    let Ok(raw) =
                        STANDARD.decode(v["media"]["payload"].as_str().unwrap_or_default())
                    else {
                        continue;
                    };
                    let now = Instant::now();
                    play_end =
                        play_end.max(now) + Duration::from_millis(raw.len() as u64 * 1000 / 8000);
                    let mut a = agent_rx
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    a.samples
                        .extend(raw.iter().map(|&b| audio::mulaw::decode(b)));
                    a.frames += 1;
                    a.last_frame = Some(now);
                    a.first_frame_after.push(now);
                }
                Some("checkpoint") => {
                    let name = v["name"].as_str().unwrap_or_default().to_owned();
                    let at = play_end;
                    let replies = replies.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep_until(at.into()).await;
                        let _ = replies
                            .send(json!({"event": "playedStream", "sequenceNumber": seq, "streamId": "sim-stream", "name": name}).to_string())
                            .await;
                    });
                }
                Some("clearAudio") => {
                    play_end = Instant::now();
                    let _ = replies
                        .send(json!({"event": "clearedAudio", "sequenceNumber": seq, "streamId": "sim-stream"}).to_string())
                        .await;
                    println!("  <- clearAudio (barge-in)");
                }
                _ => {}
            }
        }
    });

    // Sender: continuous 20 ms frames (silence unless a turn is playing).
    let (turn_tx, mut turn_rx) =
        mpsc::channel::<(Vec<u8>, tokio::sync::oneshot::Sender<Instant>)>(1);
    let frames_tx = out_tx.clone();
    let sender = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(FRAME_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
        let silence = vec![audio::mulaw::SILENCE; FRAME];
        let mut current: Option<(Vec<u8>, usize, tokio::sync::oneshot::Sender<Instant>)> = None;
        let mut chunk = 0u64;
        loop {
            tick.tick().await;
            if current.is_none() {
                if let Ok((audio, done)) = turn_rx.try_recv() {
                    current = Some((audio, 0, done));
                }
            }
            let frame: Vec<u8> = match &mut current {
                Some((audio, pos, _)) if *pos < audio.len() => {
                    let end = (*pos + FRAME).min(audio.len());
                    let mut f = audio[*pos..end].to_vec();
                    f.resize(FRAME, audio::mulaw::SILENCE);
                    *pos = end;
                    f
                }
                _ => silence.clone(),
            };
            if let Some((audio, pos, _)) = &current {
                if *pos >= audio.len() {
                    if let Some((_, _, done)) = current.take() {
                        let _ = done.send(Instant::now());
                    }
                }
            }
            chunk += 1;
            let msg = json!({
                "event": "media", "sequenceNumber": chunk + 1, "streamId": "sim-stream",
                "media": { "track": "inbound", "timestamp": (chunk * FRAME_MS).to_string(), "chunk": chunk,
                           "payload": STANDARD.encode(&frame) }
            });
            if frames_tx.send(msg.to_string()).await.is_err() {
                break;
            }
        }
    });

    // 3. Greeting.
    let greeting_ms = wait_first_audio(&agent, stream_started, Duration::from_secs(10)).await;
    match greeting_ms {
        Some(ms) => println!("greeting: first audio {ms} ms after stream start"),
        None => println!("greeting: no audio within 10 s"),
    }
    wait_idle(&agent, Duration::from_millis(900), Duration::from_secs(30)).await;

    // 4. Caller turns.
    let mut results = Vec::new();
    for (i, (mulaw, voiced_end)) in turns.into_iter().enumerate() {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let total = mulaw.len();
        turn_tx.send((mulaw, done_tx)).await?;
        let finished_at = done_rx.await.unwrap_or_else(|_| Instant::now());
        // Speech actually ended at the last voiced sample, not the end of the file.
        let trailing = Duration::from_millis(((total - voiced_end) as u64 * 1000) / 8000);
        let speech_end = finished_at.checked_sub(trailing).unwrap_or(finished_at);
        let first = wait_first_audio(
            &agent,
            speech_end,
            Duration::from_secs(args.turn_timeout_secs),
        )
        .await;
        match first {
            Some(ms) => println!("turn {}: voice-to-voice {ms} ms (client-measured)", i + 1),
            None => println!(
                "turn {}: no agent audio within {} s",
                i + 1,
                args.turn_timeout_secs
            ),
        }
        results.push(first);
        wait_idle(&agent, Duration::from_millis(1000), Duration::from_secs(30)).await;
    }

    // 5. Hang up and report.
    sender.abort();
    drop(out_tx);
    drop(turn_tx);
    tokio::time::sleep(Duration::from_millis(500)).await;
    receiver.abort();
    save_wav(
        &args.out,
        &agent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .samples,
    )?;
    println!("agent audio saved to {}", args.out.display());

    let measured: Vec<u64> = results.iter().flatten().copied().collect();
    if !measured.is_empty() {
        let mut sorted = measured.clone();
        sorted.sort_unstable();
        let pick = |p: f64| {
            sorted[((p / 100.0 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1]
        };
        println!(
            "client voice-to-voice: p50 {} ms, p95 {} ms over {} turns",
            pick(50.0),
            pick(95.0),
            sorted.len()
        );
    }

    tokio::time::sleep(Duration::from_secs(2)).await;
    let report: Value = http
        .get(format!(
            "{}/v1/calls/{call_id}",
            args.server.trim_end_matches('/')
        ))
        .header("x-api-key", &args.api_key)
        .send()
        .await?
        .json()
        .await
        .unwrap_or(Value::Null);
    println!("\nserver-side latency (ms from caller speech end):");
    for stage in [
        "stt_final_ms",
        "llm_first_token_ms",
        "tts_first_byte_ms",
        "voice_to_voice_ms",
    ] {
        let s = &report["latency"][stage];
        println!("  {stage:<20} p50 {:>6}  p95 {:>6}", s["p50"], s["p95"]);
    }
    println!("\ntranscript:");
    for t in report["transcript"].as_array().into_iter().flatten() {
        println!(
            "  [{}] {}{}",
            t["role"].as_str().unwrap_or("?"),
            t["text"].as_str().unwrap_or_default(),
            if t["interrupted"] == true {
                " (interrupted)"
            } else {
                ""
            }
        );
    }
    println!(
        "\ncost: ₹{:.4}  usage: {}",
        report["cost_inr"].as_f64().unwrap_or(0.0),
        report["usage"]
    );
    Ok(())
}

/// Waits for the first agent frame received after `since`; returns ms.
async fn wait_first_audio(
    agent: &Arc<Mutex<AgentAudio>>,
    since: Instant,
    timeout: Duration,
) -> Option<u64> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        {
            let a = agent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(t) = a.first_frame_after.iter().find(|t| **t >= since) {
                return Some(t.duration_since(since).as_millis() as u64);
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    None
}

/// Waits until the agent has sent nothing for `quiet`.
async fn wait_idle(agent: &Arc<Mutex<AgentAudio>>, quiet: Duration, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let last = agent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_frame;
        if last.is_none_or(|t| t.elapsed() >= quiet) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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
    // Linear resampling to 8 kHz (offline, not on any hot path).
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

fn save_wav(path: &PathBuf, samples: &[i16]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    Ok(())
}
