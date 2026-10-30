#!/usr/bin/env bash
# Starts a Cloudflare quick tunnel to the local server and prints the public
# URL to put in PUBLIC_BASE_URL, so Plivo can reach the webhooks and the
# media WebSocket. The tunnel adds latency; real numbers come from the VPS.
set -euo pipefail

PORT="${PORT:-8080}"

if ! command -v cloudflared >/dev/null 2>&1; then
  echo "cloudflared not found. Install it: https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/" >&2
  exit 1
fi

log="$(mktemp)"
cloudflared tunnel --no-autoupdate --url "http://localhost:${PORT}" >"$log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true; rm -f "$log"' EXIT INT TERM

url=""
for _ in $(seq 1 60); do
  url="$(grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' "$log" | head -n1 || true)"
  [[ -n "$url" ]] && break
  if ! kill -0 "$pid" 2>/dev/null; then
    cat "$log" >&2
    exit 1
  fi
  sleep 0.5
done

if [[ -z "$url" ]]; then
  echo "tunnel did not report a URL within 30 s:" >&2
  cat "$log" >&2
  exit 1
fi

echo
echo "  PUBLIC_BASE_URL=$url"
echo
echo "Put this in .env and restart voice-server. Ctrl-C stops the tunnel."
wait "$pid"
