# Hydra test harness (scripts/)

Owner: **testlab / lead**. These scripts exercise the downloader end-to-end
without the GUI. `scripts/acceptance.sh` is the acceptance gate;
`scripts/testserver.py` is the controllable local origin the headless
`speedtest` example points at.

## Running

```bash
scripts/acceptance.sh              # full gate (CDN stages need HYDRA_REF_URL; they SKIP without it)
scripts/acceptance.sh <REF_URL>    # same stages against a different target
```

Requirements: `cargo`, `python3`, `shasum`, `awk` - all base macOS tools.
Stages 1-2 need internet access to the reference CDN; every other stage is
localhost-only and never touches the network.

Scratch state lives in a `mktemp -d` directory (0700, unpredictable) that the
EXIT trap removes along with every `testserver.py` the script started. Because it is never
reused, a killed run cannot leave a partial that a later run would "resume" by
accident, and a squatted symlink has nowhere to sit.
Set `HYDRA_ACC_DIR` to keep the artefacts instead: that directory is created if
needed and deliberately NOT removed on exit.
Exit code is 0 iff the final line is `ACCEPTANCE: PASS`. Every speedtest
invocation is wrapped in a watchdog (`timeout_run`: a background sleep+kill,
since macOS has no GNU `timeout`), so a hung run cannot hang the gate.

## The local test server

```bash
python3 scripts/testserver.py --mode <mode> --port 8899 --file payload.bin \
        [--drop-p 0.15] [--stall-secs 45] [--stall-prefix 4096] [--rate-kib 0]
```

| mode | ranged GET | HEAD | purpose |
|---|---|---|---|
| `range` | `206` + `Content-Range`, `Accept-Ranges: bytes` (`416` if unsatisfiable) | **405** | forces the probe's ranged-GET fallback and exercises 206 validation |
| `norange` | always `200`, full body, `Accept-Ranges: none` | `200` | forces the single-stream fallback (`single=1`) |
| `flaky` | like `range`, but drops mid-body with probability `--drop-p` after a random 0-256 KiB prefix; never drops the same range twice in a row; 1-byte probe ranges are never dropped | 405 | exercises retry convergence |
| `oversend` | `206` whose body is LONGER than the declared `Content-Range` (`Content-Length` covers the excess, and the filler wraps around so the final chunk over-sends too) | 405 | pins the per-chunk byte cap: without one, writes spill into the next chunk |
| `stall` | headers plus `--stall-prefix` bytes, then silence for `--stall-secs` with no FIN/RST; the same range is served in full on a later request | 405 | pins the 30 s read deadline: a client without one waits for the server |
| `noadvertise` | real `206` + `Content-Range` | `200` with a truthful `Content-Length` but **no** `Accept-Ranges` | pins the active range verification (HEAD alone would settle for one connection) |
| `nolength` | no `Content-Length`, close-delimited body | `200`, no length | pins the unknown-size path (`size=null`): one sequential stream, no sidecar |

Every response carries `Content-Type: application/octet-stream` and (except
`nolength`) a true `Content-Length`. The server is threaded (16+ concurrent
connections), quiet, and robust against client aborts. The file is served at
`/` regardless of its on-disk name; suffix (`bytes=-N`) and open-ended
(`bytes=N-`) ranges are supported. `--rate-kib` throttles each connection so a
test can catch a transfer mid-flight.

## Stages

| # | stage | proves |
|---|---|---|
| 0 | build | `cargo build --release --example speedtest` produces the CLI. On failure the run stops early but still prints the summary. |
| 1 | throughput | 4 connections over the reference URL average >= 25.0 MiB/s; a 1-connection run is reported for contrast (informational) |
| 2 | resume | (a) fresh 512 MiB run -> shasum = REF; (b) delete `.part`, sidecar **and the completed file**; (c) `run_seconds=6` must print `PAUSED bytes=<n>` with n>0 and leave `.part` + meta; (d) rerun -> `resumed=1` and shasum == REF |
| 3 | norange-fallback | Range ignored -> one connection (`single=1`), bytes match |
| 4 | head-reject+ranges | HEAD 405 -> ranged-GET probe; 4 ranged connections (`single=0`), bytes match; self-checks HEAD and `Content-Range` directly |
| 5 | flaky-retry | mid-body drops at p=0.2 still converge; asserts `single=0` and bytes match |
| 6 | cancel-cleanup | throttled run cancels itself at 3 s; first proves a real `.part` + sidecar existed, then asserts `.part`, `.hydra.json` and `.hydra.json.tmp` are all gone and that bytes > 0 had landed |
| 7 | oversend-bytecap | a 206 carrying 64 KiB more than it declared still yields a byte-identical file; asserts `single=0` |
| 8 | stall-timeout | a silent origin still completes, and the elapsed time proves the deadline fired: asserting only a lower bound would also pass a client that simply waited out the server |
| 9 | probe-verify | HEAD 200 without `Accept-Ranges` is upgraded to multi-connection by the verification GET (`acceptsRanges=true`, `method=range`, size learned) |
| 10 | unknown-size | `size=null` runs as one sequential stream, writes **no** sidecar, and the bytes match |
| 11 | unit | `cargo test` passes |

## Assumptions worth knowing

1. **Data file location**: a completed run leaves its bytes at `out` minus
   `.part`. Stage 2 deletes that file before its resume leg - otherwise the
   resumed run renames to `full (2)` while the hash helper keeps returning the
   old `full`, and the byte-identity check silently compares a file with itself.
2. **Sidecar meta path**: `out` with `.part` replaced by `.hydra.json`.
3. **Stage 2 pause**: `run_seconds=6` assumes the reference target is slower
   than ~85 MiB/s so a 512 MiB run cannot finish inside 6 s. A faster server
   would finish first and the stage FAILs with a note.
4. **`bytes=` on a resumed run** is not asserted (the contract does not say
   whether it is the total on disk or only newly downloaded bytes);
   `resumed=1` plus shasum equality is the criterion.
5. **Ranged stages assert `single=0`** and the single-stream stages assert
   `single=1`: a stage that silently degraded to one connection would otherwise
   still look green.

## Harness self-verification

`python3 -m py_compile` and `bash -n` are clean; every server mode is smoke
tested on localhost (HEAD statuses, 206/Content-Range, suffix/open-ended/416
ranges, no-range 200s, oversend, stall, noadvertise, nolength, shasum equality)
with no leftover processes.
