#!/usr/bin/env bash
# Free capacity test: how many calls at once a server of a given size handles.
#
#   scripts/stress.sh                       # 2 CPU / 4 GB, 1 → 300 calls
#   SIZES="1:2g 2:4g 4:8g" scripts/stress.sh
#   STEPS="1 10 25 50 100" scripts/stress.sh
#
# Runs the real voice-server in Docker with CPU/RAM limits, fake Sarvam and
# fake Plivo, on a network with no internet. Costs nothing: it refuses to
# start unless the containers have no Sarvam/Plivo keys and cannot reach
# Sarvam, Plivo or the internet. Results go to stress-results/<time>/.
set -euo pipefail

cd "$(dirname "$0")/.."
SIZES=${SIZES:-"2:4g"}
STEPS=${STEPS:-"1 10 25 50 100 150 200 300"}
TURNS=${TURNS:-"/samples/simran_sales_1_neutral.wav /samples/simran_customer_1_neutral.wav /samples/simran_recovery_1_neutral.wav"}
OUT="stress-results/$(date +%Y%m%d-%H%M%S)"
COMPOSE=(docker compose -f docker-compose.stress.yml)
SERVER=voice-stress-voice-server-1
mkdir -p "$OUT"

say() { printf '\n== %s\n' "$*"; }
fail() { printf '\nSTOPPED: %s\n' "$*" >&2; "${COMPOSE[@]}" down -v >/dev/null 2>&1 || true; exit 1; }

if ! docker image inspect voice-stress:latest >/dev/null 2>&1 || [[ ${REBUILD:-0} == 1 ]]; then
  say "building the image (first time takes a few minutes)"
  docker build -t voice-stress:latest . >"$OUT/build.log" 2>&1 || fail "docker build failed, see $OUT/build.log"
fi

wav_args=()
for w in $TURNS; do wav_args+=(--wav "$w"); done

for size in $SIZES; do
  cpus=${size%%:*}
  mem=${size#*:}
  say "server size: $cpus CPU, $mem RAM"
  "${COMPOSE[@]}" down -v >/dev/null 2>&1 || true
  STRESS_CPUS=$cpus STRESS_MEM=$mem "${COMPOSE[@]}" up -d --quiet-pull >/dev/null 2>&1 \
    || fail "could not start the containers"

  # ---- money safety checks: all must pass before any call ----
  [[ $(docker network inspect voice-stress_offline -f '{{.Internal}}') == true ]] \
    || fail "the test network is not internal"
  if docker exec "$SERVER" env | grep -qE '^(SARVAM|PLIVO)_'; then
    fail "the test server has Sarvam/Plivo settings"
  fi
  [[ $(docker exec "$SERVER" printenv PROVIDER_MODE) == fake ]] || fail "PROVIDER_MODE is not fake"
  "${COMPOSE[@]}" run --rm -T load --check-offline || fail "the test network can reach the internet"
  echo "safety: no keys, fake providers, no internet: OK (₹0)"

  for n in $STEPS; do
    printf '  %3s calls at once ... ' "$n"
    stats="$OUT/stats-${cpus}cpu-${n}.txt"
    ( while true; do
        docker stats --no-stream --format '{{.Name}} {{.CPUPerc}} {{.MemUsage}}' \
          "$SERVER" voice-stress-postgres-1 voice-stress-minio-1 2>/dev/null
        sleep 1
      done ) >"$stats" &
    sampler=$!
    result=$("${COMPOSE[@]}" run --rm -T load --server http://voice-server:8080 \
      --calls "$n" --spread-secs 5 "${wav_args[@]}" 2>"$OUT/load-${cpus}cpu-${n}.err" | grep '^RESULT ' | cut -c8- || true)
    kill "$sampler" 2>/dev/null; wait "$sampler" 2>/dev/null || true
    [[ -n $result ]] || result='{"calls":'"$n"',"completed":0,"failed":'"$n"',"errors":["load tool crashed"]}'
    verdict=$(python3 -I scripts/stress_report.py step "$OUT" "$cpus" "$mem" "$n" "$stats" "$result")
    echo "$verdict"
    docker logs "$SERVER" >"$OUT/server-${cpus}cpu.log" 2>&1 || true
    [[ $verdict == OK* ]] || break
    sleep 5 # let post-call work finish before the next step
  done
  "${COMPOSE[@]}" down -v >/dev/null 2>&1 || true
done

python3 -I scripts/stress_report.py summary "$OUT"
echo "details: $OUT/"
