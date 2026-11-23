# Hydra - multi-stream downloader for macOS

Paste a URL, pick a folder, and Hydra pulls the file over N parallel HTTP range
connections - with resume and live progress. Native shell (Tauri v2 + system WebKit),
no bundled Chromium.

## Why

On the reference network a single TCP flow swings between 2.8 and 29.4 MiB/s within
45 seconds (measured, same machine, same URL), while 4 parallel flows sustain
25.9 MiB/s. The origin is HTTP/1.1-only (server: Nexus/1.0, no alt-svc), so browsers
open exactly one connection per download and inherit the troughs. Parallel connections
fix this: uncongested flows absorb airtime.

Acceptance run (real CDN, reference file 7.77 GB, this checkout, scripts/acceptance.sh):

| stage | result |
|---|---|
| throughput, 4 connections, 1 GiB | **44.1 MiB/s average (bar: >= 25)** |
| single connection (contrast) | 33.8 MiB/s average |
| pause -> resume, 512 MiB | byte-identical (sha1 match, paused at 298,400,748 B) |
| server ignores Range (200 fallback) | single-stream, byte-identical |
| HEAD 405 -> ranged-GET probe fallback | ok, multi-connection |
| flaky server (20% mid-body drops) | completes via retries, byte-identical |
| cancel mid-transfer (4 x 192 KiB/s for 3 s -> 2.0 MiB written) | `.part`, sidecar and sidecar temp all deleted |
| server over-sends each 206 by 64 KiB | byte-identical (per-chunk cap holds) |
| server goes silent mid-body | read deadline fires at 30 s, retry converges |
| HEAD 200 without Accept-Ranges | verification GET upgrades to 4 connections |
| server sends no Content-Length | single sequential stream, no sidecar, byte-identical |
| unit tests | 52/52 |

## Features

1. URL input -> probe on paste/enter: HEAD first, falls back to a ranged GET
   ("Range: bytes=0-0") when HEAD is rejected (403/405/501). When HEAD omits
   Accept-Ranges it verifies with one more ranged GET, so an origin that honours
   ranges but does not advertise them still gets the multi-connection speed.
2. Before you commit, the probe reports size, range support, which method decided
   it (HEAD / Range GET), the answering server, and whether a resumable partial
   already exists on disk
3. Filename from Content-Disposition (filename* and filename=), else URL path
   basename - editable before start
4. Native folder picker, remembered between launches
5. Connections 1-16 (default 8), persisted
6. Parallel ranged download; chunk = ceil(size / N), last chunk clamped to size-1
7. Live progress: rolling ~2 s speed, bytes done/total, ETA, per-connection fill bars
   (events at >= 4 Hz)
8. Pause / Resume / Cancel - pause keeps .part + meta so Resume can continue; Cancel
   deletes the half-downloaded .part, its .hydra.json sidecar and any leftover .tmp, so a
   cancelled download leaves nothing behind
9. Per-chunk retry: 5 attempts, exponential backoff (500ms * 2^n, cap 8s), resumes
   from that chunk's offset
10. Graceful fallback: server ignores Range -> single-stream, never corrupts
11. Resume across app restarts via the <file>.hydra.json sidecar (flushed at most
    1/s, atomic tmp+rename)
12. On completion: verifies bytes actually written per chunk (not just the
    preallocated file length), fsyncs, renames, deletes meta, offers Reveal in
    Finder
13. Per-chunk byte cap: a 206 that sends more than it declared cannot spill into
    a neighbouring chunk
14. 30 s idle timeout per block read, so a black-holed flow retries instead of
    hanging the worker forever
15. Human error strings: raw errno/IO text never reaches the UI

## Build

Requirements: Rust (rustup), Xcode Command Line Tools, Node (for the Tauri CLI).

    npm install            # @tauri-apps/cli
    npx tauri build        # release .app bundle
    open src-tauri/target/release/bundle/macos/Hydra.app

Dev run: npx tauri dev

## Test

The whole gate (throughput on a reference target + resume + fallback + flaky + units):

    bash scripts/acceptance.sh

The two CDN stages need a reference URL - none is committed to this repo:

    export HYDRA_REF_URL="https://your-cdn.example/big-file.bin"

Without it, stages 1-2 SKIP and the localhost + unit stages run on their own.

Local-only modes of the test server (no CDN traffic):

    python3 scripts/testserver.py --mode range|norange|flaky|oversend|stall --port 8899 --file big.bin
    src-tauri/target/release/examples/speedtest http://127.0.0.1:8899/ 4 67108864

oversend serves a 206 whose body is longer than the range it declared (pins the
per-chunk byte cap); stall goes silent after a few bytes on the first request for
each range, then serves it fully (pins the read timeout); --rate-kib throttles each
connection so a test can catch a transfer mid-flight (used by the cancel stage).

The speedtest example is the headless form of the app engine (identical core):

    speedtest <url> [conns=4] [max_bytes=1073741824] [out=/tmp/hydra-speed.part] [run_seconds=0] [cancel_after_secs=0]

run_seconds > 0 pauses mid-flight (sidecar + .part kept); rerunning with the same
url/out resumes. cancel_after_secs > 0 cancels mid-flight instead and must leave neither
.part nor sidecar behind. Final SUMMARY line is machine-readable.

## UI development without Rust

The frontend runs in a plain browser against a mock that implements the exact
Tauri contract:

    python3 -m http.server 8801
    open "http://127.0.0.1:8801/ui/index.html?demo=downloading"

Demo states: idle | probing | ready | resume | downloading | paused | done |
error | single. Add &snap=1 to resolve instantly for a deterministic screenshot.
Inside real Tauri the mock stands aside automatically (it installs
window.__TAURI__ only when the runtime has not already provided it).

## Layout

    src-tauri/src/engine/            tauri-free engine core (probe, plan, meta, names, download, settings)
    src-tauri/src/bridge.rs          Tauri commands + event emission (the only AppHandle-aware layer)
    src-tauri/examples/speedtest.rs  headless acceptance harness
    ui/                              vanilla JS + CSS single-window frontend
    scripts/                         local test server + acceptance gate
    CONTRACT.md                      the exact frontend<->backend interface

## Non-goals

Torrents/magnet links, browser extension, scheduling/queues, accounts, telemetry.

## Roadmap

Known gaps and the improvement plan (engine hardening, accessibility ports,
verification additions) live in [ROADMAP.md](ROADMAP.md).

## License

MIT - see [LICENSE](LICENSE).
