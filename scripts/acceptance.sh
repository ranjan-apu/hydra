#!/usr/bin/env bash
# =============================================================================
# Hydra acceptance harness (owner: testlab; see scripts/README-TESTS.md)
#
# Usage:
#   scripts/acceptance.sh            # run against the CONTRACT.md reference URL
#   scripts/acceptance.sh <REF_URL>  # run against a different reference target
#
# Stages (each prints PASS/FAIL; final summary table + ACCEPTANCE: PASS|FAIL):
#   0 build         - cargo build --release --example speedtest
#   1 throughput    - 4-conn avg_mibps >= 25.0 (+ single-conn, informational)
#   2 resume        - 512 MiB fresh vs paused(6s)-then-resumed, shasum equality
#   3 norange       - server ignores Range -> single-stream fallback (single=1)
#   4 head-reject   - HEAD 405 + 206 ranged GETs -> multi-conn ranged download
#   5 flaky         - mid-body drops at p=0.2 -> retries must converge
#   6 unit          - cargo test
#
# Needs: cargo, python3, shasum, awk (base macOS tools; no GNU timeout needed).
# All temp state lives under /tmp/hydra-acc/ and is removed by the cleanup
# trap, which also kills every testserver this script started. Every speedtest
# run is wrapped in a watchdog so the harness can never hang. Exit status is
# 0 iff the final line is ACCEPTANCE: PASS. If stage 0 fails, the run stops
# early (nothing else can execute) but still prints the summary table.
# =============================================================================
set -euo pipefail

# cargo lives in ~/.cargo/bin on default macOS installs; needed by stage 0/6.
export PATH="$HOME/.cargo/bin:$PATH"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TESTSERVER="$ROOT/scripts/testserver.py"
SPEEDTEST="$ROOT/src-tauri/target/release/examples/speedtest"

# Reference target from CONTRACT.md ("Reference test target" section).
DEFAULT_REF_URL="https://reference-cdn.example/dld/reference-path?token=REDACTED"

ACC_DIR="/tmp/hydra-acc"
RUN_LOG="$ACC_DIR/last-run.log"
TESTFILE="$ACC_DIR/test64.bin"
SRC_SHA=""

PORT_NORANGE=""
PORT_RANGE=""
PORT_FLAKY=""
SERVER_PIDS=""                 # space-separated pids of testserver instances

STAGE_NAMES=()
STAGE_RESULTS=()
STAGE_DETAILS=()
CURRENT_DETAIL=""
STAGE_FAIL=0

usage() {
  echo "usage: scripts/acceptance.sh [REF_URL]"
  echo "  REF_URL defaults to the CONTRACT.md reference target."
}

ARG1=""
if [ $# -ge 1 ]; then ARG1="$1"; fi
if [ "$ARG1" = "-h" ] || [ "$ARG1" = "--help" ]; then
  usage
  exit 0
fi
if [ $# -gt 1 ]; then
  usage >&2
  exit 2
fi
REF_URL="$DEFAULT_REF_URL"
if [ -n "$ARG1" ]; then REF_URL="$ARG1"; fi
case "$REF_URL" in
  http://*|https://*) ;;
  *) echo "error: REF_URL must start with http:// or https:// (got: $REF_URL)" >&2
     exit 2 ;;
esac

# ---- cleanup: kill every testserver we started, remove all temp state -----
kill_servers() {
  local pid
  for pid in $SERVER_PIDS; do
    kill "$pid" 2>/dev/null || true
  done
  for pid in $SERVER_PIDS; do
    wait "$pid" 2>/dev/null || true
  done
  SERVER_PIDS=""
}

cleanup() {
  kill_servers
  rm -rf "$ACC_DIR"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

note() { CURRENT_DETAIL="$CURRENT_DETAIL$1"; }

# ---- watchdog (macOS has no GNU timeout) ---------------------------------
# timeout_run <secs> <cmd...>: run cmd with a sleep+kill watchdog. Returns
# the command's exit status, or 124 if we had to kill it.
timeout_run() {
  local secs="$1"
  shift
  "$@" &
  local pid=$!
  local start=$SECONDS
  while kill -0 "$pid" 2>/dev/null; do
    if [ $((SECONDS - start)) -ge "$secs" ]; then
      kill -TERM "$pid" 2>/dev/null || true
      sleep 1
      kill -KILL "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      return 124
    fi
    sleep 0.25
  done
  local rc=0
  wait "$pid" || rc=$?
  return "$rc"
}

# ---- speedtest runner -----------------------------------------------------
# run_speed <secs> <speedtest args...>: watchdog-wrapped; output goes to
# $RUN_LOG and is echoed; returns the speedtest exit status.
run_speed() {
  local secs="$1"
  shift
  local rc=0
  timeout_run "$secs" "$SPEEDTEST" "$@" >"$RUN_LOG" 2>&1 || rc=$?
  cat "$RUN_LOG" || true
  return "$rc"
}

# ---- SUMMARY line parsing (CONTRACT.md "Headless speedtest harness") ------
summary_get() {  # summary_get <log> <key> -> value from the LAST SUMMARY line
  grep '^SUMMARY ' "$1" 2>/dev/null | tail -n 1 | sed -n "s/.*[[:space:]]$2=\([^[:space:]]*\).*/\1/p"
}
summary_error() {  # error=<message> may contain spaces
  grep '^SUMMARY ' "$1" 2>/dev/null | tail -n 1 | sed -n 's/.* error=\(.*\)$/\1/p'
}
paused_bytes() {
  grep '^PAUSED ' "$1" 2>/dev/null | tail -n 1 | sed -n 's/.*bytes=\([0-9][0-9]*\).*/\1/p'
}
has_resumed_line() {
  grep -q '^RESUMED ' "$1" 2>/dev/null
}
sha_of() {
  shasum "$1" | awk '{print $1}'
}
data_file() {  # data_file <out>: where a completed speedtest left its bytes.
  if [ -f "$1" ]; then        # contract: the data stays at <out> (a .part path)
    echo "$1"
  elif [ -f "${1%.part}" ]; then  # defensive: binary renamed .part away
    echo "${1%.part}"
  else
    return 1
  fi
}

# ---- port picking / server management -------------------------------------
port_is_free() {
  python3 - "$1" <<'PYEOF' 2>/dev/null
import socket
import sys
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
try:
    s.bind(("127.0.0.1", int(sys.argv[1])))
except OSError:
    sys.exit(1)
s.close()
PYEOF
}

pick_port() {  # first free port among the preferred candidates, else ephemeral
  local c
  for c in 8899 8901 8902 8903 8904 8905; do
    if port_is_free "$c"; then
      echo "$c"
      return 0
    fi
  done
  python3 - <<'PYEOF' 2>/dev/null
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PYEOF
}

wait_port() {  # wait_port <port>: up to ~15 s for the server to accept
  local port="$1" i
  for i in $(seq 1 60); do
    if python3 -c "import socket; socket.create_connection(('127.0.0.1', $port), 0.2).close()" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

start_server() {  # start_server <mode> <port> <file> [drop_p]
  local mode="$1" port="$2" file="$3" drop=""
  if [ $# -ge 4 ]; then drop="$4"; fi
  if [ -n "$drop" ]; then
    python3 "$TESTSERVER" --mode "$mode" --port "$port" --file "$file" --drop-p "$drop" &
  else
    python3 "$TESTSERVER" --mode "$mode" --port "$port" --file "$file" &
  fi
  SERVER_PIDS="$SERVER_PIDS $!"
  wait_port "$port"
}

# ---- harness self-check helper ---------------------------------------------
probe_check() {  # probe_check <url> <method> <range-or-""> -> "status k=v ..."
  local url="$1" method="$2" rng=""
  if [ $# -ge 3 ]; then rng="$3"; fi
  python3 - "$url" "$method" "$rng" <<'PYEOF' 2>/dev/null
import sys
import urllib.error
import urllib.request

url, method, rng = sys.argv[1], sys.argv[2], sys.argv[3]
req = urllib.request.Request(url, method=method)
if rng:
    req.add_header("Range", rng)
try:
    resp = urllib.request.urlopen(req, timeout=10)
    code, headers, n = resp.status, resp.headers, len(resp.read(65536))
except urllib.error.HTTPError as exc:
    code, headers, n = exc.code, exc.headers, len(exc.read(65536))
except Exception as exc:  # connection refused, timeout, ...
    print("ERROR %s" % exc)
    raise SystemExit(0)
headers = {k.lower(): v.strip() for k, v in (headers or {}).items()}
out = [str(code), "body=%d" % n]
for key in ("allow", "accept-ranges", "content-range", "content-length"):
    if key in headers:
        out.append("%s=%s" % (key, headers[key]))
print(" ".join(out))
PYEOF
}

# =============================================================================
# stage functions (each: return 0 = PASS, 1 = FAIL; details via note)
# =============================================================================

stage_build() {
  if ! command -v cargo >/dev/null 2>&1; then
    note "cargo not found (is ~/.cargo/bin on PATH?)"
    return 1
  fi
  if ! (cd "$ROOT/src-tauri" && timeout_run 900 cargo build --release --example speedtest); then
    note "cargo build --release --example speedtest failed"
    return 1
  fi
  if [ ! -x "$SPEEDTEST" ]; then
    note "binary missing after build: $SPEEDTEST"
    return 1
  fi
  note "cargo build --release --example speedtest ok"
  return 0
}

stage_throughput() {
  local ok avg err rc ok1 avg1 err1
  if ! run_speed 300 "$REF_URL" 4 1073741824 "$ACC_DIR/speed4.part" 0; then
    note "4-conn run failed or timed out (see log above)"
    return 1
  fi
  ok="$(summary_get "$RUN_LOG" ok)"
  if [ "$ok" != "1" ]; then
    err="$(summary_error "$RUN_LOG")"
    note "4-conn SUMMARY ok=$ok error=$err"
    return 1
  fi
  avg="$(summary_get "$RUN_LOG" avg_mibps)"
  if [ -z "$avg" ]; then
    note "SUMMARY is missing avg_mibps"
    return 1
  fi
  if ! awk -v a="$avg" 'BEGIN { exit !(a + 0 >= 25.0) }'; then
    note "avg_mibps=$avg < 25.0"
    return 1
  fi
  note "4conn avg_mibps=$avg (>=25.0)"
  # single-connection contrast run: informational only, never fails the stage
  rc=0
  run_speed 300 "$REF_URL" 1 268435456 "$ACC_DIR/speed1.part" 0 >/dev/null || rc=$?
  ok1="$(summary_get "$RUN_LOG" ok)"
  avg1="$(summary_get "$RUN_LOG" avg_mibps)"
  if [ "$rc" -eq 0 ] && [ "$ok1" = "1" ] && [ -n "$avg1" ]; then
    note "; 1conn info avg_mibps=$avg1"
  else
    err1="$(summary_error "$RUN_LOG")"
    note "; 1conn info failed: error=$err1 rc=$rc ok=$ok1"
  fi
  return 0
}

stage_resume() {
  local out="$ACC_DIR/full.part"
  local meta="$ACC_DIR/full.hydra.json"   # out with .part -> .hydra.json
  local ok bytes data REF_SHA pb sha ok2 res err

  # (a) fresh full run: first 512 MiB of REF_URL with 4 connections
  if ! run_speed 240 "$REF_URL" 4 536870912 "$out" 0; then
    note "fresh 512MiB run failed or timed out"
    return 1
  fi
  ok="$(summary_get "$RUN_LOG" ok)"
  bytes="$(summary_get "$RUN_LOG" bytes)"
  if [ "$ok" != "1" ]; then
    err="$(summary_error "$RUN_LOG")"
    note "fresh SUMMARY ok=$ok error=$err"
    return 1
  fi
  if [ "$bytes" != "536870912" ]; then
    note "fresh bytes=$bytes != 536870912"
    return 1
  fi
  data="$(data_file "$out")" || { note "no data file at $out"; return 1; }
  REF_SHA="$(sha_of "$data")"
  note "fresh 512MiB ok sha=$REF_SHA"

  # (b) wipe .part + sidecar meta so the next run starts fresh
  rm -f "$out" "$out.hydra.json" "$meta"
  if [ -e "$out" ] || [ -e "$meta" ]; then
    note "could not delete $out / $meta"
    return 1
  fi

  # (c) run_seconds=6: the harness must pause with bytes > 0 and keep state
  if ! run_speed 90 "$REF_URL" 4 536870912 "$out" 6; then
    note "run_seconds=6 run failed or timed out"
    return 1
  fi
  pb="$(paused_bytes "$RUN_LOG")"
  if [ -z "$pb" ] || [ "$pb" -le 0 ]; then
    note "expected 'PAUSED bytes=<n>' with n>0 (did the run complete first?)"
    return 1
  fi
  if [ ! -f "$out" ] || [ ! -f "$meta" ]; then
    note ".part/meta missing after pause (part=$( [ -f "$out" ] && echo yes || echo no ) meta=$( [ -f "$meta" ] && echo yes || echo no ))"
    return 1
  fi
  note "; paused bytes=$pb"

  # (d) same args, run to completion: resumed=1, shasum equal to (a)
  if ! run_speed 240 "$REF_URL" 4 536870912 "$out" 0; then
    note "resuming run failed or timed out"
    return 1
  fi
  ok2="$(summary_get "$RUN_LOG" ok)"
  res="$(summary_get "$RUN_LOG" resumed)"
  if [ "$ok2" != "1" ] || [ "$res" != "1" ]; then
    err="$(summary_error "$RUN_LOG")"
    note "resume SUMMARY ok=$ok2 resumed=$res error=$err"
    return 1
  fi
  data="$(data_file "$out")" || { note "no data file after resume"; return 1; }
  sha="$(sha_of "$data")"
  if [ "$sha" != "$REF_SHA" ]; then
    note "shasum mismatch: fresh=$REF_SHA resumed=$sha"
    return 1
  fi
  if has_resumed_line "$RUN_LOG"; then
    note "; resumed=1 sha=match"
  else
    note "; resumed=1 sha=match (no RESUMED line printed)"
  fi
  return 0
}

prep_testfile() {  # 64 MiB random payload, reused by stages 3-5
  local size
  if [ -f "$TESTFILE" ]; then
    size="$(wc -c <"$TESTFILE" | tr -d '[:space:]')"
    if [ "$size" != "67108864" ]; then
      rm -f "$TESTFILE"
    fi
  fi
  if [ ! -f "$TESTFILE" ]; then
    if ! timeout_run 120 head -c 67108864 /dev/urandom >"$TESTFILE"; then
      note "could not create 64MiB random file at $TESTFILE"
      return 1
    fi
  fi
  SRC_SHA="$(sha_of "$TESTFILE")"
  return 0
}

norange_checks() {
  local url="$1"
  local out="$ACC_DIR/norange.part"
  local headres rgres ok single data sha
  # harness self-checks: HEAD 200 + accept-ranges=none; ranged GET -> 200 full
  headres="$(probe_check "$url" HEAD "")"
  if [ "${headres%% *}" != "200" ]; then
    note "self-check HEAD -> $headres (want 200 + accept-ranges=none)"
    return 1
  fi
  case "$headres" in
    *"accept-ranges=none"*) ;;
    *) note "self-check HEAD missing accept-ranges=none: $headres" ; return 1 ;;
  esac
  rgres="$(probe_check "$url" GET "bytes=0-1023")"
  if [ "${rgres%% *}" != "200" ]; then
    note "self-check ranged GET -> $rgres (want 200: range ignored)"
    return 1
  fi
  # the real test
  if ! run_speed 120 "$url" 4 67108864 "$out" 0; then
    note "speedtest run failed or timed out"
    return 1
  fi
  ok="$(summary_get "$RUN_LOG" ok)"
  single="$(summary_get "$RUN_LOG" single)"
  if [ "$ok" != "1" ]; then
    note "SUMMARY ok=$ok error=$(summary_error "$RUN_LOG")"
    return 1
  fi
  if [ "$single" != "1" ]; then
    note "single=$single != 1 (no-range single-stream fallback missing)"
    return 1
  fi
  data="$(data_file "$out")" || { note "no data file at $out"; return 1; }
  sha="$(sha_of "$data")"
  if [ "$sha" != "$SRC_SHA" ]; then
    note "shasum mismatch: src=$SRC_SHA got=$sha"
    return 1
  fi
  note "single=1 sha=match port=$PORT_NORANGE"
  return 0
}

stage_norange() {
  local rc=1
  if prep_testfile && start_server norange "$PORT_NORANGE" "$TESTFILE"      && norange_checks "http://127.0.0.1:$PORT_NORANGE/"; then
    rc=0
  fi
  kill_servers
  return "$rc"
}

range_checks() {
  local url="$1"
  local out="$ACC_DIR/range.part"
  local headres rgres ok single data sha
  # harness self-checks: HEAD must be rejected, ranged GET must be a valid 206
  headres="$(probe_check "$url" HEAD "")"
  if [ "${headres%% *}" != "405" ]; then
    note "self-check HEAD -> $headres (want 405)"
    return 1
  fi
  rgres="$(probe_check "$url" GET "bytes=0-1023")"
  if [ "${rgres%% *}" != "206" ]; then
    note "self-check ranged GET -> $rgres (want 206)"
    return 1
  fi
  case "$rgres" in
    *"content-range=bytes 0-1023/67108864"*) ;;
    *) note "self-check Content-Range wrong: $rgres" ; return 1 ;;
  esac
  # the real test: probe falls back to ranged GET, download uses 4 connections
  if ! run_speed 120 "$url" 4 67108864 "$out" 0; then
    note "speedtest run failed or timed out"
    return 1
  fi
  ok="$(summary_get "$RUN_LOG" ok)"
  single="$(summary_get "$RUN_LOG" single)"
  if [ "$ok" != "1" ]; then
    note "SUMMARY ok=$ok error=$(summary_error "$RUN_LOG")"
    return 1
  fi
  if [ "$single" != "0" ]; then
    note "single=$single != 0 (ranged multi-conn path not taken)"
    return 1
  fi
  data="$(data_file "$out")" || { note "no data file at $out"; return 1; }
  sha="$(sha_of "$data")"
  if [ "$sha" != "$SRC_SHA" ]; then
    note "shasum mismatch: src=$SRC_SHA got=$sha"
    return 1
  fi
  note "single=0 sha=match port=$PORT_RANGE (HEAD->405 probe fallback)"
  return 0
}

stage_range() {
  local rc=1
  if prep_testfile && start_server range "$PORT_RANGE" "$TESTFILE"      && range_checks "http://127.0.0.1:$PORT_RANGE/"; then
    rc=0
  fi
  kill_servers
  return "$rc"
}

flaky_checks() {
  local url="$1"
  local out="$ACC_DIR/flaky.part"
  local ok single data sha
  if ! run_speed 240 "$url" 4 67108864 "$out" 0; then
    note "speedtest run failed or timed out"
    return 1
  fi
  ok="$(summary_get "$RUN_LOG" ok)"
  if [ "$ok" != "1" ]; then
    note "SUMMARY ok=$ok error=$(summary_error "$RUN_LOG")"
    return 1
  fi
  single="$(summary_get "$RUN_LOG" single)"
  data="$(data_file "$out")" || { note "no data file at $out"; return 1; }
  sha="$(sha_of "$data")"
  if [ "$sha" != "$SRC_SHA" ]; then
    note "shasum mismatch: src=$SRC_SHA got=$sha"
    return 1
  fi
  note "ok=1 single=$single sha=match port=$PORT_FLAKY (drop_p=0.2)"
  return 0
}

stage_flaky() {
  local rc=1
  if prep_testfile && start_server flaky "$PORT_FLAKY" "$TESTFILE" 0.2      && flaky_checks "http://127.0.0.1:$PORT_FLAKY/"; then
    rc=0
  fi
  kill_servers
  return "$rc"
}

stage_unit() {
  if ! (cd "$ROOT/src-tauri" && timeout_run 900 cargo test); then
    note "cargo test failed"
    return 1
  fi
  note "cargo test ok"
  return 0
}

# =============================================================================
# runner
# =============================================================================

run_stage() {  # run_stage <label> <function>
  local label="$1" fn="$2" rc=0
  echo ""
  echo "== stage: $label =="
  CURRENT_DETAIL=""
  : >"$RUN_LOG"
  "$fn" || rc=$?
  STAGE_NAMES+=("$label")
  STAGE_DETAILS+=("$CURRENT_DETAIL")
  if [ "$rc" -eq 0 ]; then
    STAGE_RESULTS+=("PASS")
    echo "==> PASS: $label  | $CURRENT_DETAIL"
  else
    STAGE_RESULTS+=("FAIL")
    STAGE_FAIL=1
    echo "==> FAIL: $label  | $CURRENT_DETAIL"
  fi
}

print_summary() {
  echo ""
  echo "=========================================================================="
  echo " Hydra acceptance - summary"
  echo "=========================================================================="
  printf '%-22s %-6s %s\n' "stage" "result" "key numbers"
  echo "--------------------------------------------------------------------------"
  local i
  for ((i = 0; i < ${#STAGE_NAMES[@]}; i++)); do
    printf '%-22s %-6s %s\n' "${STAGE_NAMES[$i]}" "${STAGE_RESULTS[$i]}" "${STAGE_DETAILS[$i]}"
  done
  echo "--------------------------------------------------------------------------"
  if [ "$STAGE_FAIL" -eq 0 ]; then
    echo "ACCEPTANCE: PASS"
  else
    echo "ACCEPTANCE: FAIL"
  fi
}

# =============================================================================
# main
# =============================================================================

echo "Hydra acceptance harness"
echo "  repo      : $ROOT"
echo "  ref url   : $REF_URL"
echo "  speedtest : $SPEEDTEST"
echo "  temp dir  : $ACC_DIR (removed on exit)"

# preflight
for cmd in python3 shasum awk; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: missing dependency: $cmd" >&2
    exit 2
  fi
done
if [ ! -f "$TESTSERVER" ]; then
  echo "error: missing $TESTSERVER" >&2
  exit 2
fi
mkdir -p "$ACC_DIR"

PORT_NORANGE="$(pick_port)"
PORT_RANGE="$(pick_port)"
PORT_FLAKY="$(pick_port)"

run_stage "0 build" stage_build
if [ "${STAGE_RESULTS[0]}" = "FAIL" ]; then
  # Without the binary no stage can run; report and stop early.
  print_summary
  exit 1
fi

run_stage "1 throughput" stage_throughput
run_stage "2 resume" stage_resume
run_stage "3 norange-fallback" stage_norange
run_stage "4 head-reject+ranges" stage_range
run_stage "5 flaky-retry" stage_flaky
run_stage "6 unit" stage_unit

print_summary
exit "$STAGE_FAIL"
