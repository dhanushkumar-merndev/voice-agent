#!/usr/bin/env bash
# Prints every cost for a month: AI services, phone calls, number rent, GST.
# Usage: ./scripts/costs.sh [YYYY-MM]   (default: this month, India time)
set -euo pipefail

cd "$(dirname "$0")/.."
key="$(grep '^API_KEY=' .env | cut -d= -f2-)"
port="$(grep '^BIND_ADDR=' .env | sed -E 's/.*:([0-9]+)$/\1/')"
query=""
[[ $# -gt 0 ]] && query="?month=$1"

curl -sf "http://localhost:${port:-8080}/v1/costs${query}" -H "X-API-Key: ${key}" | jq -r '
  "Costs for \(.month)  (₹, excluding GST unless marked)",
  "",
  "  calls placed / answered   \(.calls) / \(.answered_calls)",
  "  talk time                 \(.talk_minutes) min",
  "",
  "  AI (Sarvam)",
  "    speech-to-text          \(.ai.stt)",
  "    text-to-speech          \(.ai.tts)",
  "    LLM                     \(.ai.llm)",
  "    lead extraction         \(.ai.extraction)",
  "    subtotal                \(.ai.total)",
  "",
  "  Phone (Plivo)",
  "    call charges            \(.telephony.calls)\(if .telephony.calls_waiting_for_plivo_cost > 0 then "  (\(.telephony.calls_waiting_for_plivo_cost) call(s) not billed by Plivo yet)" else "" end)",
  "    number rent             \(.telephony.number_rental)",
  "    subtotal                \(.telephony.total)",
  "",
  "  TOTAL                     \(.total_excl_gst)",
  "  TOTAL incl. \(.gst_percent)% GST      \(.total_incl_gst)",
  "",
  "  per talk minute           \(.per_talk_minute_excl_gst // "-") (AI + phone, excl. rent)",
  "  per answered call         \(.per_answered_call_excl_gst.ai_and_phone // "-") (AI + phone), \(.per_answered_call_excl_gst.including_number_rental // "-") with rent"
'
