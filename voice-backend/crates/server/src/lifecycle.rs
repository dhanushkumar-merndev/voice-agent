//! Server lifecycle: active-call accounting and graceful shutdown.
//!
//! On SIGTERM/Ctrl-C the server starts *draining*: `/readyz` returns 503 and
//! new calls are rejected, but HTTP and WebSocket traffic for calls already
//! in progress keeps flowing. Once every active call has ended, or the
//! grace period expires, the shutdown token is cancelled (call actors watch
//! it and hang up) and the HTTP server stops.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use common::metric_names;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Shared lifecycle state. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Lifecycle {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    draining: AtomicBool,
    active_calls: AtomicUsize,
    shutdown: CancellationToken,
}

impl Lifecycle {
    /// `true` once shutdown has started.
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    /// Stops accepting new calls.
    pub fn start_draining(&self) {
        self.inner.draining.store(true, Ordering::Release);
    }

    /// Number of calls currently holding a [`CallGuard`].
    pub fn active_calls(&self) -> usize {
        self.inner.active_calls.load(Ordering::Acquire)
    }

    /// Registers an active call; the call counts until the guard is dropped.
    pub fn call_started(&self) -> CallGuard {
        let now = self.inner.active_calls.fetch_add(1, Ordering::AcqRel) + 1;
        metrics::gauge!(metric_names::ACTIVE_CALLS).set(now as f64);
        CallGuard {
            lifecycle: self.clone(),
        }
    }

    /// Token cancelled when the server must stop; call actors end their call on it.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.inner.shutdown.clone()
    }

    /// Waits until no calls are active or `grace` elapses. Returns `true`
    /// if all calls finished in time.
    pub async fn wait_idle(&self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        loop {
            if self.active_calls() == 0 {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Full shutdown sequence, used as axum's graceful-shutdown future:
    /// wait for a signal, drain, wait for calls, then cancel the rest.
    pub async fn drain_on_signal(self, grace: Duration) {
        wait_for_signal().await;
        self.start_draining();
        tracing::info!(
            active_calls = self.active_calls(),
            grace_secs = grace.as_secs(),
            "shutdown requested; draining active calls"
        );
        if self.wait_idle(grace).await {
            tracing::info!("all calls finished");
        } else {
            tracing::warn!(
                active_calls = self.active_calls(),
                "grace period expired; ending remaining calls"
            );
        }
        self.inner.shutdown.cancel();
    }
}

/// Marks one active call. Dropping it decrements the count.
#[derive(Debug)]
pub struct CallGuard {
    lifecycle: Lifecycle,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let now = self
            .lifecycle
            .inner
            .active_calls
            .fetch_sub(1, Ordering::AcqRel)
            .saturating_sub(1);
        metrics::gauge!(metric_names::ACTIVE_CALLS).set(now as f64);
    }
}

/// Resolves on Ctrl-C or SIGTERM.
async fn wait_for_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guards_track_active_calls() {
        let lc = Lifecycle::default();
        let a = lc.call_started();
        let b = lc.call_started();
        assert_eq!(lc.active_calls(), 2);
        drop(a);
        assert_eq!(lc.active_calls(), 1);
        assert!(!lc.wait_idle(Duration::from_millis(50)).await);
        drop(b);
        assert!(lc.wait_idle(Duration::from_millis(50)).await);
    }
}
