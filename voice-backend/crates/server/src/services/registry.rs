//! In-memory registry of calls that have been created but not finished.
//!
//! It carries what the webhooks and the media WebSocket need between
//! requests: the pipeline parameters, connections opened while ringing,
//! the Plivo CallUUID and the command channel of the running call.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use common::ids::{CallId, KbId, TenantId};
use dashmap::DashMap;
use pipeline::{CallCommand, CallParams, Prewarmed};
use tokio::sync::mpsc;

/// One call in flight.
#[derive(Debug)]
pub struct CallSlot {
    /// Call ID.
    pub id: CallId,
    /// Tenant.
    pub tenant: TenantId,
    /// Knowledge base bound to this call.
    pub kb_id: Option<KbId>,
    /// Pipeline parameters.
    pub params: CallParams,
    /// Max duration in seconds (for Plivo time limits).
    pub max_duration_secs: u32,
    /// Simulator call (no Plivo leg).
    pub simulated: bool,
    /// When the slot was created (stale-slot sweeping).
    pub created: Instant,
    /// Plivo CallUUID once known.
    pub plivo_uuid: Mutex<Option<String>>,
    /// Plivo request UUID from placing the call; cancels it while it rings.
    pub request_uuid: Mutex<Option<String>>,
    /// Connections opened during ringing, taken by the media stream.
    pub prewarmed: tokio::sync::Mutex<Option<Prewarmed>>,
    /// Commands to the running call.
    pub commands: Mutex<Option<mpsc::Sender<CallCommand>>>,
    /// `true` once a media stream has attached.
    pub streaming: std::sync::atomic::AtomicBool,
}

impl CallSlot {
    /// Plivo CallUUID, if known.
    pub fn plivo_uuid(&self) -> Option<String> {
        self.plivo_uuid
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Plivo request UUID, once the call is placed.
    pub fn request_uuid(&self) -> Option<String> {
        self.request_uuid
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Records the Plivo request UUID.
    pub fn set_request_uuid(&self, uuid: &str) {
        *self
            .request_uuid
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(uuid.to_owned());
    }

    /// Records the Plivo CallUUID.
    pub fn set_plivo_uuid(&self, uuid: &str) {
        *self
            .plivo_uuid
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(uuid.to_owned());
    }

    /// Sends a command to the running call; `false` if it is not running.
    pub fn command(&self, cmd: CallCommand) -> bool {
        let guard = self
            .commands
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.as_ref().is_some_and(|tx| tx.try_send(cmd).is_ok())
    }
}

/// All calls in flight.
#[derive(Debug, Default)]
pub struct CallRegistry {
    calls: DashMap<CallId, Arc<CallSlot>>,
}

impl CallRegistry {
    /// Adds a call.
    pub fn insert(&self, slot: Arc<CallSlot>) {
        self.calls.insert(slot.id, slot);
    }

    /// Looks up a call.
    pub fn get(&self, id: CallId) -> Option<Arc<CallSlot>> {
        self.calls.get(&id).map(|e| Arc::clone(e.value()))
    }

    /// Removes a call.
    pub fn remove(&self, id: CallId) -> Option<Arc<CallSlot>> {
        self.calls.remove(&id).map(|(_, v)| v)
    }

    /// Number of calls in flight.
    pub fn len(&self) -> usize {
        self.calls.len()
    }

    /// `true` when no calls are in flight.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Drops slots that never streamed and are older than `max_age`
    /// (calls that were never answered and whose hangup webhook was lost).
    pub fn sweep(&self, max_age: std::time::Duration) -> usize {
        let before = self.calls.len();
        self.calls.retain(|_, slot| {
            slot.streaming.load(std::sync::atomic::Ordering::Relaxed)
                || slot.created.elapsed() < max_age
        });
        before - self.calls.len()
    }
}
