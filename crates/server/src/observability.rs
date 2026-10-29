//! Logging (`tracing`, JSON by default) and the Prometheus recorder.

use common::config::LogFormat;
use metrics_exporter_prometheus::{BuildError, Matcher, PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Histogram buckets (ms) for every metric whose name ends in `_ms`.
/// Dense around the 300-1200 ms latency targets.
const LATENCY_BUCKETS_MS: &[f64] = &[
    10.0, 25.0, 50.0, 100.0, 150.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0, 1000.0,
    1200.0, 1500.0, 2000.0, 3000.0, 5000.0, 10000.0,
];

/// Buckets for characters spoken per assistant turn.
const SPOKEN_CHARS_BUCKETS: &[f64] = &[20.0, 50.0, 80.0, 120.0, 160.0, 200.0, 250.0, 300.0];

/// Installs the global tracing subscriber. `RUST_LOG` overrides the default filter.
pub fn init_tracing(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,sqlx=warn,aws_config=warn,aws_smithy_runtime=warn")
    });
    let registry = tracing_subscriber::registry().with(filter);
    match format {
        LogFormat::Json => registry
            .with(
                fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_current_span(true),
            )
            .init(),
        LogFormat::Pretty => registry.with(fmt::layer()).init(),
    }
}

/// Installs the global Prometheus recorder and registers metric help texts.
pub fn install_metrics() -> Result<PrometheusHandle, BuildError> {
    let handle = prometheus_builder()?.install_recorder()?;
    common::metric_names::describe_all();
    Ok(handle)
}

/// A recorder handle that is not installed globally (for tests).
pub fn test_metrics_handle() -> Result<PrometheusHandle, BuildError> {
    Ok(prometheus_builder()?.build_recorder().handle())
}

fn prometheus_builder() -> Result<PrometheusBuilder, BuildError> {
    PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Suffix("_ms".to_owned()), LATENCY_BUCKETS_MS)?
        .set_buckets_for_metric(
            Matcher::Full(common::metric_names::SPOKEN_CHARS_PER_TURN.to_owned()),
            SPOKEN_CHARS_BUCKETS,
        )
}

/// Spawns the periodic upkeep the Prometheus recorder needs to drain
/// histogram buffers between scrapes.
pub fn spawn_metrics_upkeep(handle: PrometheusHandle) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            handle.run_upkeep();
        }
    });
}
