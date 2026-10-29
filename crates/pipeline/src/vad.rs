//! Voice activity detection on a dedicated thread pool.
//!
//! ONNX inference never runs on the async executor. A fixed set of OS
//! threads each own one model instance; every call is pinned to one
//! worker, which keeps that call's recurrent state. The async side only
//! sends refcounted μ-law frames and receives probabilities over channels.
//!
//! Silero VAD v5 at 8 kHz takes 256-sample windows (32 ms) plus 32 samples
//! of context, a `[2, 1, 128]` recurrent state and the sample rate.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc as std_mpsc,
    },
    time::Instant,
};

use bytes::Bytes;
use tokio::sync::mpsc;

/// Samples per VAD window at 8 kHz.
pub const WINDOW: usize = 256;
/// Window duration in milliseconds.
pub const WINDOW_MS: u32 = 32;
const CONTEXT: usize = 32;
const STATE_LEN: usize = 2 * 128;

/// VAD setup failure.
#[derive(Debug, thiserror::Error)]
pub enum VadError {
    /// The model could not be loaded.
    #[error("loading VAD model {path}: {message}")]
    Load {
        /// Model path.
        path: PathBuf,
        /// Error message.
        message: String,
    },
}

/// Per-call recurrent state.
#[derive(Debug, Clone)]
pub struct VadState {
    state: Vec<f32>,
    context: Vec<f32>,
}

impl Default for VadState {
    fn default() -> Self {
        Self {
            state: vec![0.0; STATE_LEN],
            context: vec![0.0; CONTEXT],
        }
    }
}

/// A VAD model instance (owned by one worker thread).
pub trait VadModel: Send {
    /// Speech probability for one window of [`WINDOW`] samples in `[-1, 1)`.
    fn predict(&mut self, state: &mut VadState, window: &[f32]) -> f32;
}

/// Creates one model per worker thread.
pub trait VadModelFactory: Send + Sync {
    /// Builds a model instance.
    fn create(&self) -> Result<Box<dyn VadModel>, VadError>;
}

/// Silero VAD (ONNX) factory.
#[derive(Debug, Clone)]
pub struct SileroFactory {
    path: PathBuf,
}

impl SileroFactory {
    /// Uses the model at `path` (`models/silero_vad.onnx`).
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
        }
    }
}

impl VadModelFactory for SileroFactory {
    fn create(&self) -> Result<Box<dyn VadModel>, VadError> {
        let load = |e: ort::Error| VadError::Load {
            path: self.path.clone(),
            message: e.to_string(),
        };
        let session = ort::session::Session::builder()
            .map_err(load)?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(load)?
            .with_intra_threads(1)
            .map_err(load)?
            .commit_from_file(&self.path)
            .map_err(load)?;
        Ok(Box::new(Silero {
            session,
            input: vec![0.0; CONTEXT + WINDOW],
            warned: false,
        }))
    }
}

struct Silero {
    session: ort::session::Session,
    input: Vec<f32>,
    warned: bool,
}

impl Silero {
    fn run(&mut self, state: &mut VadState, window: &[f32]) -> Result<f32, ort::Error> {
        self.input.clear();
        self.input.extend_from_slice(&state.context);
        self.input.extend_from_slice(window);
        let input =
            ort::value::Tensor::from_array(([1usize, CONTEXT + WINDOW], self.input.clone()))?;
        let st = ort::value::Tensor::from_array(([2usize, 1, 128], state.state.clone()))?;
        let sr = ort::value::Tensor::from_array((Vec::<i64>::new(), vec![8000i64]))?;
        let outputs = self
            .session
            .run(ort::inputs!["input" => input, "state" => st, "sr" => sr])?;
        let (_, prob) = outputs["output"].try_extract_tensor::<f32>()?;
        let p = prob.first().copied().unwrap_or(0.0);
        let (_, new_state) = outputs["stateN"].try_extract_tensor::<f32>()?;
        if new_state.len() == STATE_LEN {
            state.state.copy_from_slice(new_state);
        }
        state
            .context
            .copy_from_slice(&self.input[self.input.len() - CONTEXT..]);
        Ok(p)
    }
}

impl VadModel for Silero {
    fn predict(&mut self, state: &mut VadState, window: &[f32]) -> f32 {
        match self.run(state, window) {
            Ok(p) => p,
            Err(e) => {
                if !self.warned {
                    tracing::error!(error = %e, "VAD inference failed");
                    self.warned = true;
                }
                0.0
            }
        }
    }
}

/// Energy-threshold VAD for tests and the offline simulator: RMS above
/// 0.02 counts as speech.
#[derive(Debug, Clone, Default)]
pub struct EnergyFactory;

struct Energy;

impl VadModel for Energy {
    fn predict(&mut self, _state: &mut VadState, window: &[f32]) -> f32 {
        let rms = (window.iter().map(|s| s * s).sum::<f32>() / window.len().max(1) as f32).sqrt();
        if rms > 0.02 {
            0.95
        } else {
            0.02
        }
    }
}

impl VadModelFactory for EnergyFactory {
    fn create(&self) -> Result<Box<dyn VadModel>, VadError> {
        Ok(Box::new(Energy))
    }
}

/// One VAD result.
#[derive(Debug, Clone, Copy)]
pub struct VadOut {
    /// Speech probability for the window.
    pub prob: f32,
    /// When the frame that completed the window was received.
    pub at: Instant,
}

enum VadMsg {
    Register { id: u64, out: mpsc::Sender<VadOut> },
    Frame { id: u64, frame: Bytes, at: Instant },
    Unregister { id: u64 },
}

/// The worker pool.
#[derive(Debug)]
pub struct VadPool {
    workers: Vec<std_mpsc::Sender<VadMsg>>,
    next_worker: AtomicUsize,
    next_id: AtomicU64,
}

impl VadPool {
    /// Starts `threads` workers, each with its own model.
    pub fn new(factory: &dyn VadModelFactory, threads: usize) -> Result<Self, VadError> {
        let mut workers = Vec::with_capacity(threads.max(1));
        for n in 0..threads.max(1) {
            let model = factory.create()?;
            let (tx, rx) = std_mpsc::channel::<VadMsg>();
            std::thread::Builder::new()
                .name(format!("vad-{n}"))
                .spawn(move || worker(model, rx))
                .map_err(|e| VadError::Load {
                    path: PathBuf::new(),
                    message: e.to_string(),
                })?;
            workers.push(tx);
        }
        Ok(Self {
            workers,
            next_worker: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
        })
    }

    /// Registers a call and returns its stream.
    pub fn open(&self) -> VadStream {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let n = self.next_worker.fetch_add(1, Ordering::Relaxed) % self.workers.len();
        let tx = self.workers[n].clone();
        let (out_tx, out_rx) = mpsc::channel(128);
        let _ = tx.send(VadMsg::Register { id, out: out_tx });
        VadStream {
            id,
            tx,
            results: out_rx,
        }
    }
}

/// One call's connection to its VAD worker. Dropping it frees the state.
#[derive(Debug)]
pub struct VadStream {
    id: u64,
    tx: std_mpsc::Sender<VadMsg>,
    /// Probabilities, one per 32 ms window.
    pub results: mpsc::Receiver<VadOut>,
}

impl VadStream {
    /// Queues a μ-law frame for analysis.
    pub fn push(&self, frame: Bytes) {
        let _ = self.tx.send(VadMsg::Frame {
            id: self.id,
            frame,
            at: Instant::now(),
        });
    }
}

impl Drop for VadStream {
    fn drop(&mut self) {
        let _ = self.tx.send(VadMsg::Unregister { id: self.id });
    }
}

impl std::fmt::Debug for VadMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VadMsg")
    }
}

struct CallVad {
    state: VadState,
    samples: Vec<f32>,
    out: mpsc::Sender<VadOut>,
}

fn worker(mut model: Box<dyn VadModel>, rx: std_mpsc::Receiver<VadMsg>) {
    let mut calls: HashMap<u64, CallVad> = HashMap::new();
    while let Ok(msg) = rx.recv() {
        match msg {
            VadMsg::Register { id, out } => {
                calls.insert(
                    id,
                    CallVad {
                        state: VadState::default(),
                        samples: Vec::with_capacity(WINDOW * 2),
                        out,
                    },
                );
            }
            VadMsg::Unregister { id } => {
                calls.remove(&id);
            }
            VadMsg::Frame { id, frame, at } => {
                let Some(call) = calls.get_mut(&id) else {
                    continue;
                };
                audio::mulaw::decode_to_f32(&frame, &mut call.samples);
                while call.samples.len() >= WINDOW {
                    let prob = model.predict(&mut call.state, &call.samples[..WINDOW]);
                    call.samples.drain(..WINDOW);
                    // A full channel means the call loop is behind; drop rather than block.
                    let _ = call.out.try_send(VadOut { prob, at });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn energy_vad_through_pool() {
        let pool = VadPool::new(&EnergyFactory, 2).unwrap();
        let mut stream = pool.open();
        // 512 samples of silence then 512 of a loud square wave.
        for _ in 0..4 {
            stream.push(Bytes::from(vec![audio::mulaw::SILENCE; 128]));
        }
        let loud: Vec<u8> = (0..128)
            .map(|i| audio::mulaw::encode(if i % 2 == 0 { 8000 } else { -8000 }))
            .collect();
        for _ in 0..4 {
            stream.push(Bytes::from(loud.clone()));
        }
        let mut probs = Vec::new();
        for _ in 0..4 {
            probs.push(stream.results.recv().await.unwrap().prob);
        }
        assert!(probs[0] < 0.5 && probs[1] < 0.5);
        assert!(probs[2] > 0.5 && probs[3] > 0.5);
    }

    #[test]
    fn silero_model_loads_if_present() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/silero_vad.onnx");
        if !path.exists() {
            eprintln!("skipping: {} not downloaded", path.display());
            return;
        }
        let mut model = SileroFactory::new(&path).create().unwrap();
        let mut state = VadState::default();
        let silence = vec![0.0f32; WINDOW];
        assert!(model.predict(&mut state, &silence) < 0.5);
    }
}
