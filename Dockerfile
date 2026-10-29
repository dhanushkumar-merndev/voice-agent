# syntax=docker/dockerfile:1.7

# ---- build -------------------------------------------------------------------
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin voice-server --bin voice-sim \
 && cp target/release/voice-server target/release/voice-sim /usr/local/bin/

# ---- runtime -----------------------------------------------------------------
# debian-slim rather than distroless: ONNX Runtime (ort) needs glibc + libstdc++.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /app voice
WORKDIR /app
COPY --from=builder /usr/local/bin/voice-server /usr/local/bin/voice-sim /usr/local/bin/
COPY prompts ./prompts
COPY config ./config
ENV BIND_ADDR=0.0.0.0:8080 \
    PROMPTS_DIR=/app/prompts \
    CONFIG_DIR=/app/config \
    MODELS_DIR=/app/models
USER voice
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/voice-server"]
