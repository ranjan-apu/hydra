# Hydra test harness (scripts/)

Owner: **testlab**. These scripts exercise the downloader end-to-end without
the GUI. `scripts/acceptance.sh` is the acceptance gate the orchestrator
(lead) runs once the Rust backend builds; `scripts/testserver.py` is the
controllable local server the harness points the headless `speedtest`
example at. Neither script touches anything outside `scripts/` and
`/tmp/hydra-acc/`.

## Running

```bash
scripts/acceptance.sh              # full acceptance, CONTRACT.md reference URL
scripts/acceptance.sh <REF_URL>    # same stages against a different target
```

Requirements: `cargo` (`~/.cargo/bin` is added to PATH automatically),
`python3`, `shasum`, `awk` - all base macOS tools. Stages 1-2 need
internet access to the reference CDN; stages 3-5 are localhost-only and never
touch the network. All scratch state is written under `/tmp/hydra-acc/` and
deleted on exit, together with every `testserver.py` the script started.
Exit code is 0 iff the final line is `ACCEPTANCE: PASS`. Every speedtest
invocation is wrapped in a watchdog (`timeout_run`: a background
sleep+kill, since macOS has no GNU `timeout`), so a hung run can never hang
the acceptance run itself.

## The local test server

```bash
python3 scripts/testserver.py --mode <range|norange|flaky> --port 8899 \
        --file payload.bin [--drop-p 0.15]
```

| mode | ranged GET | HEAD | purpose |
|---|---|---|---|
| `range` | `206` + `Content-Range`, `Accept-Ranges: bytes` (`416` if unsatisfiable) | **405** | forces the probe's ranged-GET fallback and exercises 206 validation |
| `norange` | always `200`, full body, `Accept-Ranges: none` | `200` (true headers) | forces the single-stream fallback (`single=1`) |
| `flaky` | like `range`, but drops the connection mid-body with probability `--drop-p` after a random 0-256 KiB prefix; never drops the same byte-range twice in a row; 1-byte ranges are never dropped | 405 | exercises retry logic until it converges |

Every response carries `Content-Type: application/octet-stream`, a true
`Content-Length`, and `Connection: close`. The server is threaded (16+
concurrent connections), quiet, and robust against client aborts. The file is
served at `/` regardless of its on-disk name; suffix (`bytes=-N`) and
open-ended (`bytes=N-`) ranges are supported.

## Stages

| # | stage | proves |
|---|---|---|
| 0 | build | `cargo build --release --example speedtest` produces the CLI binary. On failure the run stops early (nothing else can execute) but still prints the summary table. |
| 1 | throughput | 4 connections over the reference URL average >= 25.0 MiB/s (`avg_mibps` from `SUMMARY`); a 1-connection run is reported for contrast (informational, cannot fail the stage) |
| 2 | resume | (a) fresh 512 MiB run -> shasum = REF; (b) delete `.part` + meta; (c) `run_seconds=6` must print `PAUSED bytes=<n>` with n>0 and leave `.part` + meta; (d) rerun to completion -> `SUMMARY resumed=1` and shasum == REF |
| 3 | norange-fallback | server ignores Range -> engine falls back to one connection (`single=1`) and the bytes still match |
| 4 | head-reject+ranges | HEAD 405 -> probe falls back to ranged GET; download runs 4 ranged connections (`single=0`), bytes match. The harness also self-checks HEAD=405 and `206` + `Content-Range: bytes 0-1023/67108864` directly against the server |
| 5 | flaky-retry | with mid-body drops at p=0.2 the engine retries ranges to completion, bytes match |
| 6 | unit | `cargo test` passes |

## Assumptions the orchestrator should double-check

1. **Data file location**: a completed run is expected to leave its bytes at
   the `out` path (`.../full.part`, per CONTRACT.md's speedtest section);
   if the binary instead renames `.part` away on completion, the harness
   falls back to `out` minus `.part` before hashing.
2. **Sidecar meta path**: `out` with `.part` replaced by `.hydra.json`
   (CONTRACT.md). Stage 2 asserts this exact path exists after a pause.
3. **Stage 2 pause**: `run_seconds=6` assumes the reference target is slower
   than ~85 MiB/s so the 512 MiB run cannot finish inside 6 s (CONTRACT.md
   observes ~25.9 MiB/s sustained). A much faster server would finish before
   pausing and the stage would FAIL with a note.
4. **`bytes=` on a resumed run** is not asserted (the contract does not say
   whether it is the total on disk or only the newly downloaded bytes);
   `resumed=1` plus shasum equality is the criterion.
5. **Flaky convergence**: the server never drops the same range twice in a
   row, so an engine that re-requests the identical range converges in one
   retry; one that resumes from the received offset converges statistically
   (0.2^5 < 0.001 per chunk within a 5-retry budget).
6. Stage 4 asserts `single=0` (multi-connection ranged path) and stage 3
   asserts `single=1` per the CONTRACT.md fallback rules.

## Harness self-verification (testlab)

Before handing over, testlab verified: `python3 -m py_compile` and
`bash -n` clean; a localhost smoke run of every server mode (HEAD statuses,
206/Content-Range, suffix/open-ended/416 ranges, no-range 200s, flaky
drop + no-repeat rule, shasum equality) with no leftover processes; and a
full dry-run of `acceptance.sh` against a contract-conformant stub
speedtest (both the PASS and the FAIL propagation paths) using only
localhost servers.
