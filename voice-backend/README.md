# voice-backend

Ultra-low-latency AI voice calling backend in Rust.

- **Telephony:** outbound calls go out through Plivo. The audio is a bidirectional μ-law 8 kHz stream, so it is never transcoded.
- **Agent:** runs on Sarvam: STT `saaras:v4` (realtime), LLM `sarvam-105b-conversations` and TTS `bulbul:v4-flash`.
- **Knowledge base:** local bge-m3 embeddings (ONNX) searched with pgvector HNSW.
- **Recordings:** stored in S3-compatible storage.

## Quick start (local)

```sh
cp ../.env.example ../.env        # one env for both apps; boots without keys
./scripts/download_models.sh      # Silero VAD + bge-m3 (~590 MB) into models/
docker compose --env-file ../.env up -d   # Postgres 16 + pgvector, MinIO (pgsty/minio build)
cargo run --release --bin voice-server
```

Then check it:

```sh
curl -s localhost:8080/healthz
curl -s localhost:8080/readyz
curl -s -H 'X-API-Key: change-me' localhost:8080/v1/ping
```

### Try the full pipeline with no API keys

Set `PROVIDER_MODE=fake` in the root `.env`. This uses scripted STT, LLM and TTS with real VAD and playout. Then run:

```sh
cargo run --release --bin voice-sim -- --api-key change-me --wav turn1.wav --wav turn2.wav
```

`voice-sim` connects to `/plivo/stream/{call_id}` like Plivo does:
- It streams each WAV file as a caller turn (any sample rate, 16-bit) in real time.
- It answers checkpoints and saves the agent's audio to `sim-out/agent.wav`.
- It prints client-measured and server-measured latency, the transcript and the cost.

With real Sarvam keys (`PROVIDER_MODE=sarvam`), use recordings of real speech.

### A real phone call

1. Run `./scripts/dev_tunnel.sh`. Put the printed URL in `PUBLIC_BASE_URL` and restart the server.
2. Set `PLIVO_AUTH_ID`, `PLIVO_AUTH_TOKEN`, `PLIVO_FROM_NUMBER` and `SARVAM_API_KEY`.
3. Place the call:

```sh
curl -s -X POST localhost:8080/v1/calls -H 'X-API-Key: change-me' -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: demo-1' -d '{
    "to": "+919876543210", "lead_name": "Ravi", "language": "auto",
    "system_prompt": "You call clinic owners for Star Growth Hub. We help clinics get more patients through ads, social media and a booking system. Find out what they need most and offer to send details on WhatsApp.",
    "knowledge_base_id": null
  }'
curl -s -H 'X-API-Key: change-me' localhost:8080/v1/calls/<call_id> | jq
```

The tunnel adds latency. Real numbers come from an India VPS.

### Knowledge base

```sh
curl -s -X POST localhost:8080/v1/knowledge-bases -H 'X-API-Key: change-me' -H 'Content-Type: application/json' -d '{"name":"Clinic FAQ"}'
curl -s -X POST localhost:8080/v1/knowledge-bases/<kb_id>/documents -H 'X-API-Key: change-me' -F file=@faq.pdf
curl -s localhost:8080/v1/knowledge-bases/<kb_id>/documents -H 'X-API-Key: change-me'
curl -s -X POST localhost:8080/v1/knowledge-bases/<kb_id>/search -H 'X-API-Key: change-me' -H 'Content-Type: application/json' -d '{"query":"price of ads","top_k":3}'
```

## API

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/v1/calls` | Place a call (`agent_id`, or `persona`/`system_prompt`; request fields override the agent's). Rate limited; supports `Idempotency-Key`. |
| GET | `/v1/calls?limit=&offset=&status=&agent_id=` | Newest calls first, numbers masked to the last 4 digits, with cost, interest and response time. |
| GET | `/v1/calls/{id}` | Status, transcript, extracted fields (agent fields under `custom`), follow-ups, recording URL (signed, 5 min), latency p50/p95 per stage, usage and cost (₹). |
| POST | `/v1/calls/{id}/hangup` | End the call. |
| GET | `/v1/stats?days=30` | Dashboard: totals, cost by service, per-day series (India dates), outcomes, languages, interest, per-agent table, latency percentiles. |
| GET | `/v1/options` | Personas, languages and voices, models, limits and whether calling is allowed right now. |
| GET, POST | `/v1/agents` | List or create agents: a prompt, typed fields to collect (`text`, `number`, `date`, `boolean`, `choice`), a knowledge base and call defaults. |
| GET, PUT, DELETE | `/v1/agents/{id}` | Read, replace or delete an agent. Past calls keep the fields they used. |
| GET, POST | `/v1/knowledge-bases` | List (with document and chunk counts) or create knowledge bases. |
| DELETE | `/v1/knowledge-bases/{id}` | Delete with its documents, chunks and stored files. |
| POST, GET | `/v1/knowledge-bases/{id}/documents` | Upload (`.txt`, `.md`, `.pdf`; processed in the background) or list documents. |
| DELETE | `/v1/knowledge-bases/{id}/documents/{doc}` | Delete one document and its chunks. |
| POST | `/v1/knowledge-bases/{id}/search` | Debug search. |
| POST | `/v1/dev/sim-calls` | `APP_ENV=local` only: a call without Plivo, for `voice-sim`. |
| POST | `/plivo/{answer,ring,hangup,recording,stream-status}/{id}` | Plivo webhooks. `X-Plivo-Signature-V3` is checked; unsigned requests are rejected. |
| WS | `/plivo/stream/{id}?t=<token>` | Bidirectional call audio. The token is per call, derived with HMAC. |
| GET | `/healthz`, `/readyz`, `/metrics` | Health checks and Prometheus metrics. |

Errors are returned as `{"error": {"code": "...", "message": "..."}}`. The `code` values are stable:
- `bad_request`, `unauthorized`, `invalid_signature`, `not_found`, `conflict`, `payload_too_large`, `rate_limited`
- `outside_call_hours`, `provider_not_configured`, `provider_error`, `unavailable`, `internal`

## How a call flows

1. **`POST /v1/calls`**
   - Synthesizes or loads the greeting (the AI and recording disclosure comes first) into the TTS cache.
   - Runs a knowledge-base search on the system prompt and lead name, and adds the results to the agent's context.
   - Dials through Plivo.
2. **Ringing (`ring_url`):** opens the Sarvam STT socket and two TTS sockets (active plus a hot spare), and warms the LLM connection.
3. **Answer:** returns the `<Stream bidirectional keepCallAlive>` XML and starts recording. Plivo's default recording limit is 60 s, so the limit is set to the call's max duration.
4. **Media stream:** the cached greeting starts playing immediately.
5. **Each turn:**
   - **Speech detection:** Silero VAD runs on a dedicated thread pool. Audio goes to Sarvam STT only while the caller speaks, plus a 300 ms pre-roll; Sarvam bills per second.
   - **LLM:** the STT final transcript starts the LLM stream with tools (`search_knowledge`, `send_details`, `end_call`). The fixed speaking rules always come first in the prompt.
   - **Text shaping:** strips markdown, emoji and URLs. Links, emails and long numbers are replaced by the "I'll send that on WhatsApp" line.
   - **Clause chunking:** a clause ends at `. ? ! । ,` (a comma only after 4+ words).
   - **Length limits:** a cap of 250 spoken characters per turn and a 4000-character TTS budget per call.
   - **Speech out:** each clause is streamed to Bulbul as soon as it forms, and audio is sent to Plivo in paced 20 ms frames.
6. **Barge-in:** caller speech for 120 ms or more while the agent talks triggers `clearAudio`, cancels the LLM and swaps the TTS socket for the spare. Only what was actually played is kept in the history.
7. **Silence and limits:** 7 s of silence → one re-prompt; silence again → goodbye and hang up. At `max_duration_secs` the agent says a polite closing line.
8. **LLM resilience:** if the primary model produces no first token within 1.5 s, or returns 5xx/429, the turn moves to `gemma4`. If that fails too, the agent says "one moment please" and retries the primary. If everything fails, it plays the apology line.
9. **After the call:**
   - The transcript and per-turn latency are saved, along with usage (STT seconds, TTS characters, LLM tokens by model) and cost in ₹ from `config/pricing.toml`.
   - `sarvam-105b` extracts lead fields (`json_schema`, validated with serde).
   - The recording is moved to `tenants/{t}/calls/{id}.mp3` and deleted from Plivo.

## Configuration

Everything comes from environment variables; see the root `.env.example` (shared with the dashboard). Static assets:

| Path | Contents |
| --- | --- |
| `prompts/speaking_rules.txt` | Fixed guardrails, always placed before the per-call prompt. |
| `prompts/lines/<lang>.toml` | Fixed spoken lines. `en-IN` is the fallback. `hi`, `ta`, `te`, `kn` and `ml` are included and should get a native-speaker review. |
| `config/voices.toml` | Language → TTS model and persona speaker. Malayalam and Odia use `bulbul:v3`, since v4 Flash has no voices for them. |
| `config/pricing.toml` | Sarvam ₹ rates used for cost tracking. |

## Tests

```sh
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                     # the smoke test starts pgvector with Docker (testcontainers)
TEST_DATABASE_URL=postgres://... cargo test --workspace   # use an existing pgvector DB instead of Docker
```

What's covered:
- **Audio and Plivo:** μ-law round trip, WAV stripping, framing, Plivo and Sarvam message serde, webhook signatures.
- **Text and prompts:** shaper (English, Hindi and Tamil clauses; markdown; URL, email and phone detection; caps and budget), prompt assembly.
- **Knowledge base:** chunking, plus real bge-m3 ingest and search when the model and DB are available.
- **Silero:** model inference.
- **Pipeline (fake providers):** a normal turn with latency marks and STT gating, barge-in, silence re-prompt then hangup, LLM timeout → fallback → "one moment" → retry, TTS budget closing, the `send_details` tool, max duration, and eager mode.
- **HTTP (smoke test):** migrations, HNSW search, auth, validation, idempotency and rejection of unsigned webhooks.

### Capacity (stress) test, free

How many calls at once a server size handles, with no Sarvam or Plivo cost:

```sh
scripts/stress.sh                                  # 2 CPU / 4 GB, 1 → 300 calls at once
SIZES="1:2g 2:4g 4:8g" scripts/stress.sh            # compare server sizes
STEPS="1 10 50" REBUILD=1 scripts/stress.sh         # rebuild the image after code changes
```

It runs the real `voice-server` image in Docker with CPU/RAM limits (`docker-compose.stress.yml`), fake Sarvam
(`PROVIDER_MODE=fake` with realistic delays, `FAKE_*_MS`) and fake Plivo (`voice-load`: real-time 20 ms media,
3 caller turns per call). Before any call it checks that the containers have no Sarvam/Plivo keys and cannot
reach the internet, and stops otherwise. Its own Postgres and MinIO are deleted afterwards. Each step is judged
against the 1-call run (reply time, audio stutter, RAM); results land in `stress-results/<time>/summary.md`.
Not covered: Sarvam's own concurrency limits for your plan, and TLS/network cost to real Sarvam (a little more
CPU than the fakes), so leave ~30 % headroom.

## Deploy to the VPS (India region)

1. Copy the repo and a production root `.env`:
   - set `APP_ENV=production` and `COMPOSE_PROFILES=app`
   - set new `PUBLIC_BASE_URL` (https), `DATABASE_URL` and `S3_*` values (Tigris or AWS S3)
   - set an `API_KEY` of at least 16 characters
2. Run `./scripts/download_models.sh`, then `docker compose --env-file ../.env up -d --build`.
3. Put Caddy in front with `deploy/Caddyfile` (automatic HTTPS; WebSockets pass through).

Monitoring: import `deploy/grafana/voice-backend.json` into Grafana. Prometheus scrapes `localhost:8080/metrics`; see `deploy/prometheus.yml`.

The server runs with host networking, so there is no Docker NAT hop on the media path. Shutdown is graceful: `/readyz` turns 503, new calls are refused, and active calls get `SHUTDOWN_GRACE_SECS` to finish.
