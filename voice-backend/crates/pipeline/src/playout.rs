//! Paced audio playout.
//!
//! Audio is sent to telephony in 20 ms frames at real-time pace, at most
//! `lead` ahead of the playback clock. Keeping the far-end queue short makes
//! `clearAudio` effective on barge-in and lets us estimate what the caller
//! actually heard.
//!
//! Audio is organised in *clauses* (IDs chosen by the caller). Clauses play
//! strictly in registration order, so a cached fixed line queued after
//! streaming TTS clauses waits its turn. After a clause's last frame we send
//! a checkpoint named `c<id>`.

use std::{collections::VecDeque, time::Duration};

use audio::{FRAME_MS, FRAME_SAMPLES};
use bytes::{Bytes, BytesMut};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use crate::types::MediaOut;

/// Commands to the playout task.
#[derive(Debug)]
pub enum PlayoutCmd {
    /// Reserve a slot for clause `id` (audio may follow later).
    Register(u64),
    /// Audio for a registered clause.
    Audio(u64, Bytes),
    /// No more audio for clause `id`.
    End(u64),
    /// A complete clip played ahead of every clause that has not started
    /// yet (a filler while a slow reply is still coming).
    Jump(u64, Bytes),
    /// Drop everything (barge-in) and report what was played.
    Clear(oneshot::Sender<Vec<PlayedClause>>),
}

/// How much of a clause was heard when playback was cleared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayedClause {
    /// Clause ID.
    pub id: u64,
    /// Fraction heard, 0..=1.
    pub fraction: f32,
}

/// Events from the playout task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayoutEvent {
    /// The first frame of clause `id` was sent.
    Started(u64),
    /// Every registered clause has been sent and (by our clock) played.
    Drained,
    /// Clause `id` ran out of audio mid-way (TTS slower than playback): the
    /// caller hears a break.
    Gap(u64),
}

#[derive(Debug)]
struct Slot {
    id: u64,
    audio: BytesMut,
    ended: bool,
    /// Bytes of this clause already sent.
    sent: usize,
    /// Playback-clock offset (ms) at which this clause started.
    start_ms: Option<u64>,
}

/// Handle to a running playout task.
#[derive(Debug, Clone)]
pub struct Playout {
    /// Command channel.
    pub tx: mpsc::Sender<PlayoutCmd>,
}

impl Playout {
    /// Starts the task. `out` receives frames, checkpoints and clears.
    pub fn spawn(
        out: mpsc::Sender<MediaOut>,
        lead: Duration,
    ) -> (Self, mpsc::Receiver<PlayoutEvent>) {
        let (tx, rx) = mpsc::channel(512);
        let (evt_tx, evt_rx) = mpsc::channel(64);
        tokio::spawn(run(rx, out, evt_tx, lead));
        (Self { tx }, evt_rx)
    }

    /// Sends a command (ignoring a stopped task).
    pub async fn send(&self, cmd: PlayoutCmd) {
        let _ = self.tx.send(cmd).await;
    }

    /// Queues a complete clip as one clause.
    pub async fn play_clip(&self, id: u64, audio: Bytes) {
        self.send(PlayoutCmd::Register(id)).await;
        self.send(PlayoutCmd::Audio(id, audio)).await;
        self.send(PlayoutCmd::End(id)).await;
    }

    /// Plays a complete clip before any clause that has not started.
    pub async fn jump_clip(&self, id: u64, audio: Bytes) {
        self.send(PlayoutCmd::Jump(id, audio)).await;
    }

    /// Clears playback and returns what was heard of each clause.
    pub async fn clear(&self) -> Vec<PlayedClause> {
        let (tx, rx) = oneshot::channel();
        self.send(PlayoutCmd::Clear(tx)).await;
        rx.await.unwrap_or_default()
    }
}

async fn run(
    mut rx: mpsc::Receiver<PlayoutCmd>,
    out: mpsc::Sender<MediaOut>,
    events: mpsc::Sender<PlayoutEvent>,
    lead: Duration,
) {
    let lead_ms = lead.as_millis() as u64;
    let mut slots: VecDeque<Slot> = VecDeque::new();
    // Playback clock: `clock_start` is when sent audio position 0 began playing.
    let mut clock_start: Option<Instant> = None;
    let mut sent_ms: u64 = 0;
    let mut drained_reported = true;

    loop {
        // Finished clauses: pop them and mark the position for Plivo.
        while slots
            .front()
            .is_some_and(|s| s.ended && s.sent >= s.audio.len())
        {
            if let Some(done) = slots.pop_front() {
                if out
                    .send(MediaOut::Checkpoint(format!("c{}", done.id)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }

        let now = Instant::now();
        let elapsed_ms = clock_start.map_or(0, |s| now.duration_since(s).as_millis() as u64);

        // Underrun: everything sent has played; restart the clock on next audio.
        if clock_start.is_some() && elapsed_ms >= sent_ms {
            let head_waiting = slots.front().is_some_and(|s| s.sent >= s.audio.len());
            if slots.is_empty() || head_waiting {
                if let Some(s) = slots.front().filter(|s| s.start_ms.is_some() && !s.ended) {
                    let _ = events.try_send(PlayoutEvent::Gap(s.id));
                }
                clock_start = None;
                sent_ms = 0;
                if slots.is_empty() && !drained_reported {
                    drained_reported = true;
                    let _ = events.send(PlayoutEvent::Drained).await;
                }
            }
        }

        // Send frames while we are less than `lead` ahead of the clock.
        loop {
            let ahead = sent_ms.saturating_sub(
                clock_start.map_or(0, |s| Instant::now().duration_since(s).as_millis() as u64),
            );
            if clock_start.is_some() && ahead >= lead_ms {
                break;
            }
            let Some(slot) = slots.front_mut() else { break };
            let available = slot.audio.len() - slot.sent;
            if available == 0 {
                if slot.ended {
                    let name = format!("c{}", slot.id);
                    slots.pop_front();
                    if out.send(MediaOut::Checkpoint(name)).await.is_err() {
                        return;
                    }
                    continue;
                }
                break;
            }
            // Wait for a full frame unless the clause is complete.
            if available < FRAME_SAMPLES && !slot.ended {
                break;
            }
            let n = available.min(FRAME_SAMPLES);
            let frame = Bytes::copy_from_slice(&slot.audio[slot.sent..slot.sent + n]);
            if slot.start_ms.is_none() {
                slot.start_ms = Some(sent_ms);
                let _ = events.try_send(PlayoutEvent::Started(slot.id));
            }
            slot.sent += n;
            if clock_start.is_none() {
                clock_start = Some(Instant::now());
            }
            sent_ms += (n as u64 * u64::from(FRAME_MS)) / FRAME_SAMPLES as u64;
            drained_reported = false;
            let finished = slot.ended && slot.sent >= slot.audio.len();
            let finished_id = slot.id;
            if out.send(MediaOut::Play(frame)).await.is_err() {
                return;
            }
            if finished {
                slots.pop_front();
                if out
                    .send(MediaOut::Checkpoint(format!("c{finished_id}")))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }

        // Sleep until the next frame is due, a command arrives, or the
        // queue's audio finishes playing.
        let wake = clock_start.map(|s| {
            let target = sent_ms.saturating_sub(lead_ms.min(sent_ms));
            // A frame is ready to go: wake when the lead allows it. Otherwise
            // (waiting for TTS) wake when queued audio finishes playing.
            let frame_ready = slots.front().is_some_and(|s| {
                let available = s.audio.len() - s.sent;
                available >= FRAME_SAMPLES || (s.ended && available > 0)
            });
            let due = if frame_ready { target } else { sent_ms };
            s + Duration::from_millis(due)
        });
        let cmd = match wake {
            Some(at) => tokio::select! {
                cmd = rx.recv() => cmd,
                () = tokio::time::sleep_until(at.max(Instant::now() + Duration::from_millis(1))) => continue,
            },
            None => rx.recv().await,
        };
        let Some(cmd) = cmd else { return };
        match cmd {
            PlayoutCmd::Register(id) => slots.push_back(Slot {
                id,
                audio: BytesMut::new(),
                ended: false,
                sent: 0,
                start_ms: None,
            }),
            PlayoutCmd::Audio(id, data) => {
                if let Some(slot) = slots.iter_mut().find(|s| s.id == id) {
                    slot.audio.extend_from_slice(&data);
                }
            }
            PlayoutCmd::End(id) => {
                if let Some(slot) = slots.iter_mut().find(|s| s.id == id) {
                    slot.ended = true;
                }
            }
            PlayoutCmd::Jump(id, data) => {
                let at = slots
                    .iter()
                    .position(|s| s.sent == 0)
                    .unwrap_or(slots.len());
                slots.insert(
                    at,
                    Slot {
                        id,
                        audio: BytesMut::from(&data[..]),
                        ended: true,
                        sent: 0,
                        start_ms: None,
                    },
                );
            }
            PlayoutCmd::Clear(reply) => {
                let elapsed =
                    clock_start.map_or(0, |s| Instant::now().duration_since(s).as_millis() as u64);
                let report = slots
                    .iter()
                    .filter_map(|s| {
                        let start = s.start_ms?;
                        let total_ms =
                            (s.audio.len() as u64 * u64::from(FRAME_MS)) / FRAME_SAMPLES as u64;
                        let heard = elapsed.saturating_sub(start).min(total_ms);
                        let fraction = if total_ms == 0 {
                            0.0
                        } else {
                            heard as f32 / total_ms as f32
                        };
                        Some(PlayedClause { id: s.id, fraction })
                    })
                    .collect();
                slots.clear();
                clock_start = None;
                sent_ms = 0;
                drained_reported = true;
                let _ = out.send(MediaOut::Clear).await;
                let _ = reply.send(report);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn paces_frames_in_order_and_reports_drain() {
        let (out_tx, mut out_rx) = mpsc::channel(1024);
        let (playout, mut events) = Playout::spawn(out_tx, Duration::from_millis(60));
        playout.send(PlayoutCmd::Register(1)).await;
        playout.send(PlayoutCmd::Register(2)).await;
        // Clause 2 has audio first but must wait for clause 1.
        playout
            .send(PlayoutCmd::Audio(2, Bytes::from(vec![2u8; 320])))
            .await;
        playout.send(PlayoutCmd::End(2)).await;
        playout
            .send(PlayoutCmd::Audio(1, Bytes::from(vec![1u8; 400])))
            .await;
        playout.send(PlayoutCmd::End(1)).await;

        let start = Instant::now();
        let mut frames = Vec::new();
        let mut checkpoints = Vec::new();
        while checkpoints.len() < 2 {
            match out_rx.recv().await.unwrap() {
                MediaOut::Play(f) => frames.push(f),
                MediaOut::Checkpoint(n) => checkpoints.push(n),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(checkpoints, vec!["c1", "c2"]);
        assert_eq!(frames.len(), 5); // 160+160+80 for clause 1, then 2x160
        assert_eq!(frames[2].len(), 80);
        assert!(frames[0].iter().all(|&b| b == 1));
        assert!(frames[3].iter().all(|&b| b == 2));
        assert_eq!(events.recv().await, Some(PlayoutEvent::Started(1)));
        assert_eq!(events.recv().await, Some(PlayoutEvent::Started(2)));
        assert_eq!(events.recv().await, Some(PlayoutEvent::Drained));
        // 90 ms of audio with 60 ms lead: drained after ~90 ms, not instantly.
        assert!(start.elapsed() >= Duration::from_millis(80));
    }

    #[tokio::test]
    async fn a_jump_clip_plays_before_a_clause_still_waiting_for_audio() {
        let (out_tx, mut out_rx) = mpsc::channel(1024);
        let (playout, mut events) = Playout::spawn(out_tx, Duration::from_millis(60));
        // Clause 1 is registered but its TTS has not answered yet.
        playout.send(PlayoutCmd::Register(1)).await;
        playout.jump_clip(9, Bytes::from(vec![9u8; 320])).await;
        assert_eq!(events.recv().await, Some(PlayoutEvent::Started(9)));
        playout
            .send(PlayoutCmd::Audio(1, Bytes::from(vec![1u8; 320])))
            .await;
        playout.send(PlayoutCmd::End(1)).await;
        assert_eq!(events.recv().await, Some(PlayoutEvent::Started(1)));
        let mut checkpoints = Vec::new();
        while checkpoints.len() < 2 {
            if let Some(MediaOut::Checkpoint(n)) = out_rx.recv().await {
                checkpoints.push(n);
            }
        }
        assert_eq!(checkpoints, vec!["c9", "c1"]);
    }

    #[tokio::test]
    async fn clear_reports_partial_playback() {
        let (out_tx, mut out_rx) = mpsc::channel(1024);
        let (playout, _events) = Playout::spawn(out_tx, Duration::from_millis(40));
        // 1 s clip.
        playout.play_clip(7, Bytes::from(vec![0u8; 8000])).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let report = playout.clear().await;
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].id, 7);
        assert!(
            report[0].fraction > 0.2 && report[0].fraction < 0.45,
            "{report:?}"
        );
        // A Clear was sent after some frames.
        let mut saw_clear = false;
        while let Ok(m) = out_rx.try_recv() {
            if matches!(m, MediaOut::Clear) {
                saw_clear = true;
            }
        }
        assert!(saw_clear);
    }

    #[tokio::test]
    async fn running_out_of_audio_mid_clause_is_reported_as_a_gap() {
        let (out_tx, mut out_rx) = mpsc::channel(1024);
        let (playout, mut events) = Playout::spawn(out_tx, Duration::from_millis(40));
        tokio::spawn(async move { while out_rx.recv().await.is_some() {} });
        playout.send(PlayoutCmd::Register(3)).await;
        // 40 ms of audio, then TTS stalls for longer than that.
        playout
            .send(PlayoutCmd::Audio(3, Bytes::from(vec![0u8; 320])))
            .await;
        assert_eq!(events.recv().await, Some(PlayoutEvent::Started(3)));
        let gap = tokio::time::timeout(Duration::from_millis(500), events.recv()).await;
        assert_eq!(gap.ok().flatten(), Some(PlayoutEvent::Gap(3)));
        playout
            .send(PlayoutCmd::Audio(3, Bytes::from(vec![0u8; 160])))
            .await;
        playout.send(PlayoutCmd::End(3)).await;
        let drained = tokio::time::timeout(Duration::from_millis(500), events.recv()).await;
        assert_eq!(drained.ok().flatten(), Some(PlayoutEvent::Drained));
    }
}
