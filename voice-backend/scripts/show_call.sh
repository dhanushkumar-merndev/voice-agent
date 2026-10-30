#!/usr/bin/env bash
# Prints a readable summary of one call: transcript, extracted lead fields,
# follow-ups, latency, usage and cost, and saves the recording to recordings/.
# Usage: ./scripts/show_call.sh <call_id>   (reads API_KEY and BIND_ADDR from ../.env)
set -euo pipefail

cd "$(dirname "$0")/.."
id="${1:?usage: $0 <call_id>}"
key="$(grep '^API_KEY=' ../.env | cut -d= -f2-)"
port="$(grep '^BIND_ADDR=' ../.env | sed -E 's/.*:([0-9]+)$/\1/')"

json="$(curl -sf "http://localhost:${port:-8080}/v1/calls/${id}" -H "X-API-Key: ${key}")"
jq -r <<<"$json" '
  "Call \(.call_id)  status=\(.status)  end=\(.end_reason // "-")  duration=\(.duration_secs // 0)s  language=\(.detected_language // .language)",
  "",
  "TRANSCRIPT",
  (.transcript[] | select(.role != "tool") |
    "  \(if .role == "user" then "Caller" else "Agent " end): \(.text)\(if .live_text != null and .live_text != .text then "  (live: \(.live_text))" else "" end)\(if .interrupted then "  [interrupted]" else "" end)"),
  "",
  "LEAD",
  (.extracted_fields // {} | to_entries[] | "  \(.key): \(.value // "-" | tostring)"),
  "",
  "FOLLOW-UPS",
  ((.follow_ups // [])[] | "  \(.topic)"),
  "",
  "APPOINTMENTS BOOKED",
  ((.appointments // [])[] | "  \(.starts_at | sub("\\+00:00$"; "Z") | fromdateiso8601? // .starts_at | if type == "number" then (. + 19800 | strftime("%a %d %b %I:%M %p IST")) else . end)  \(.name)  (\(.duration_mins) min)"),
  "",
  "LATENCY (ms, p50 / p95)",
  "  voice-to-voice  \(.latency.voice_to_voice_ms.p50 // "-") / \(.latency.voice_to_voice_ms.p95 // "-")",
  "  stt final       \(.latency.stt_final_ms.p50 // "-") / \(.latency.stt_final_ms.p95 // "-")",
  "  llm 1st token   \(.latency.llm_first_token_ms.p50 // "-") / \(.latency.llm_first_token_ms.p95 // "-")",
  "",
  "USAGE",
  "  stt \(.usage.stt_seconds // 0)s   tts \(.usage.tts_chars // 0) chars",
  ((.usage.llm // {}) | to_entries[] | "  \(.key): \(.value.prompt_tokens) in (\(.value.cached_prompt_tokens) cached), \(.value.completion_tokens) out"),
  "",
  "COST (₹)",
  "  speech-to-text   \(.cost.ai.stt)",
  "  re-transcription \(.cost.ai.stt_retranscribe // 0)  (caller turns, batch STT after the call)",
  "  text-to-speech   \(.cost.ai.tts)  (\(.cost.ai.tts_cached_chars) chars played from cache, free)",
  "  LLM              \(.cost.ai.llm)",
  "  lead extraction  \(.cost.ai.extraction)",
  "  phone call       \(if .cost.telephony.call == null then (if .cost.telephony.pending then "waiting for Plivo (re-run in a minute)" else "-" end) else "\(.cost.telephony.call)  (\(.cost.telephony.billed_secs)s at ₹\(.cost.telephony.rate_per_min * 10000 | round / 10000)/min)" end)",
  "  total            \(.cost.total_excl_gst)  +\(.cost.gst_percent)% GST = \(.cost.total_incl_gst)",
  "  per minute       \(.cost.per_minute_excl_gst // "-")   (unused paid phone seconds: \(.cost.telephony.unused_paid_secs // "-"))",
  "",
  "RECORDING"
'

# The signed link expires in 5 minutes, so save a local copy instead.
url="$(jq -r '.recording_url // empty' <<<"$json")"
if [[ -n "$url" ]]; then
  mkdir -p recordings
  curl -sf -o "recordings/${id}.mp3" "$url"
  echo "  saved to $(pwd)/recordings/${id}.mp3"
else
  echo "  not stored yet (it arrives about 20 s after the call ends)"
fi
