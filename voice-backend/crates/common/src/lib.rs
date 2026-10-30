//! Shared building blocks for the voice backend: typed configuration,
//! API errors, identifiers, the PostgreSQL pool, the S3-compatible storage
//! client, metric names and static assets (speaking rules, fixed lines,
//! voice map, pricing).

pub mod assets;
pub mod config;
pub mod db;
pub mod error;
pub mod ids;
pub mod mask;
pub mod metric_names;
pub mod secret;
pub mod storage;

pub use config::Config;
pub use error::{AppError, ErrorCode};
