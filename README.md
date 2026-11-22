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
| unit tests | 36/36 |

## Features

1. URL input -> probe on paste/enter: HEAD first, falls back to a ranged GET
   ("Range: bytes=0-0") when HEAD is rejected (403/405/501)
2. Shows total size, range support, resolved filename after probe
3. Filename from Content-Disposition (filename* and filename=), else URL path
   basename - editable before start
4. Native folder picker, remembered between launches
5. Connections 1-16 (default 8), persisted
6. Parallel ranged download; chunk = ceil(size / N), last chunk clamped to size-1
7. Live progress: rolling ~2 s speed, bytes done/total, ETA, per-connection fill bars
   (events at >= 4 Hz)
8. Pause / Resume / Cancel - cancel deletes .part + meta
9. Per-chunk retry: 5 attempts, exponential backoff (500ms * 2^n, cap 8s), resumes
   from that chunk's offset
10. Graceful fallback: server ignores Range -> single-stream, never corrupts
11. Resume across app restarts via the <file>.hydra.json sidecar (flushed at most
    1/s, atomic tmp+rename)
12. On completion: verifies final size, deletes meta, offers Reveal in Finder

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

    python3 scripts/testserver.py --mode range|norange|flaky --port 8899 --file big.bin
    src-tauri/target/release/examples/speedtest http://127.0.0.1:8899/ 4 67108864

The speedtest example is the headless form of the app engine (identical core):

    speedtest <url> [conns=4] [max_bytes=1073741824] [out=/tmp/hydra-speed.part] [run_seconds=0]

run_seconds > 0 pauses mid-flight (sidecar + .part kept); rerunning with the same
url/out resumes. Final SUMMARY line is machine-readable.

## UI development without Rust

The frontend runs in a plain browser against a mock that implements the exact
Tauri contract:

    python3 -m http.server 8801
    open "http://127.0.0.1:8801/ui/index.html?demo=downloading"

Demo states: ready | downloading | paused | done | error | unknown | single |
resume | noranges | folderprompt | probing - plus URL trigger words (see
ui/mock-tauri.js for the full set). Inside real Tauri the mock stands aside
automatically (window.__TAURI_INTERNALS__ check).

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
