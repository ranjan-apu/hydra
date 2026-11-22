# Contributing to Hydra

## Dev setup

    npm install                     # @tauri-apps/cli
    npx tauri dev                   # development window
    cd src-tauri && cargo test      # hermetic unit tests

The frontend runs without Rust against the contract mock:

    python3 -m http.server 8801
    open "http://127.0.0.1:8801/ui/index.html?demo=downloading"

## Ground rules

- **CONTRACT.md is the canonical frontend<->backend interface.** If you change a
  command, an event payload, or the sidecar schema, update CONTRACT.md in the
  same PR and keep both sides in sync.
- **The engine core stays tauri-free.** Nothing under src-tauri/src/engine/ may
  import tauri or touch an AppHandle; platform glue lives in bridge.rs.
- **Engine changes need a test.** Chunk math, parsing, and meta round-trips are
  pure functions - easy to pin. Behavioral changes should extend
  scripts/acceptance.sh (the localhost stages run with no CDN access; the two
  CDN stages need HYDRA_REF_URL and SKIP without it).
- **Human error strings are part of the product**: lowercase, human-readable,
  shown verbatim by the UI. No internal Rust error dumps.
- No unsafe, no new heavy dependencies, no telemetry.

## Before opening a PR

1. `cargo test` green
2. `cargo check --lib --bins --examples` clean
3. relevant acceptance stages passing locally
4. UI changes: verified in the mock (all states reachable via ?demo=)

See ROADMAP.md for the current known gaps - fixes there are very welcome.
