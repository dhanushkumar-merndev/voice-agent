#!/usr/bin/env bash
# Places a test call from a template in templates/calls/, waits for it to
# end, then prints the transcript, booking, latency and cost (show_call.sh).
# Usage: ./scripts/call.sh <template> <phone>    e.g. ./scripts/call.sh 10 9036215854
#        ./scripts/call.sh list
# <template> is a file name, or just its number prefix ("10" -> 10_v4_...).
# A 10-digit phone gets +91 in front.  (reads API_KEY and BIND_ADDR from .env)
set -euo pipefail

cd "$(dirname "$0")/.."
dir=templates/calls
if [[ "${1:-}" == "list" || $# -lt 2 ]]; then
  # Estimated ₹ per talk minute, before GST: phone ~0.43 + speech-to-text
  # ~0.25 + ~600 spoken characters at the voice model's rate + LLM tokens
  # (~9k input, mostly cached, and ~100 output per minute) at its rates.
  rate() { awk -v m="[$1.\"$2\"]" -v k="$3" '$0 == m { f = 1; next } /^\[/ { f = 0 } f && $1 == k { print $3; exit }' config/pricing.toml; }
  printf '%-36s %-24s %-16s %-26s %-6s %s\n' TEMPLATE VOICE "VOICE MODEL" LLM LANG "EST ₹/MIN"
  for f in "$dir"/*.json; do
    tts="$(jq -r '.tts_model // "bulbul:v4-flash"' "$f")"
    llm="$(jq -r '.llm_model // "sarvam-105b-conversations"' "$f")"
    est="$(awk -v t="$(rate tts "$tts" inr_per_1k_chars)" -v i="$(rate llm "$llm" input_per_1m)" \
      -v c="$(rate llm "$llm" cached_input_per_1m)" -v o="$(rate llm "$llm" output_per_1m)" \
      'BEGIN { printf "~%.2f", 0.68 + 0.6 * t + (1500 * i + 7500 * c + 100 * o) / 1e6 }')"
    jq -r --arg f "$(basename "$f" .json)" --arg est "$est" --arg tts "$tts" --arg llm "$llm" \
      '"\($f)\t\(.voice // "default")\t\($tts)\t\($llm)\t\(.language)\t\($est)"' "$f" |
      awk -F'\t' '{ printf "%-36s %-24s %-16s %-26s %-6s %s\n", $1, $2, $3, $4, $5, $6 }'
  done
  echo "(estimate excl. 18% GST and the ₹200/month number rent; the real cost prints after each call)"
  [[ "${1:-}" == "list" ]] || echo "usage: $0 <template> <phone>"
  exit 0
fi

tpl="$1"
[[ -f "$tpl" ]] || tpl="$(ls "$dir"/"$1"*.json 2>/dev/null | head -1)"
[[ -f "$tpl" ]] || { echo "no template matching '$1' (try: $0 list)"; exit 1; }
to="$(tr -d ' -' <<<"$2")"
[[ "$to" =~ ^[0-9]{10}$ ]] && to="+91$to"
[[ "$to" =~ ^91[0-9]{10}$ ]] && to="+$to"

key="$(grep '^API_KEY=' .env | cut -d= -f2-)"
port="$(grep '^BIND_ADDR=' .env | sed -E 's/.*:([0-9]+)$/\1/')"
base="http://localhost:${port:-8080}"

body="$(jq --arg to "$to" '. + {to: $to}' "$tpl")"
resp="$(curl -s -X POST "$base/v1/calls" -H "X-API-Key: $key" -H "Content-Type: application/json" \
  -H "Idempotency-Key: test-$(date +%s%N)" -d "$body")"
id="$(jq -r '.call_id // empty' <<<"$resp")"
[[ -n "$id" ]] || { echo "call not placed: $resp"; exit 1; }
echo "calling ${to:0:3}******${to: -4} with $(basename "$tpl" .json)  (call $id)"

status=queued
while :; do
  status="$(curl -s "$base/v1/calls/$id" -H "X-API-Key: $key" | jq -r '.status')"
  case "$status" in completed|failed|no_answer|busy|canceled|cancelled) break ;; esac
  sleep 2
done
echo "call $status; waiting for Plivo's charge and the recording..."
sleep 25
./scripts/show_call.sh "$id"
