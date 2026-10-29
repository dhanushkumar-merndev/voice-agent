//! The `voice-server` application: REST API, Plivo webhooks and media
//! WebSocket, health checks and Prometheus metrics.
//!
//! [`bootstrap::build_state`] wires providers, models and caches from
//! config; [`app::build_router`] assembles every route; `main.rs` adds the
//! database, migrations and graceful shutdown around them.

pub mod app;
pub mod bootstrap;
pub mod error;
pub mod lifecycle;
pub mod middleware;
pub mod observability;
pub mod routes;
pub mod services;
pub mod state;
