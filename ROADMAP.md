# Hydra - Roadmap: known gaps and inspiration

This document consolidates every weakness found in this build by a full code audit
(plus an independent read-only auditor pass on 2026-11-22), and the concrete things
worth porting from two sibling builds of the same spec that were audited alongside
it: **hydra-pi** (the most careful engine details + best UI accessibility) and
**hydra-deepseek** (the most distinctive visual design + nicest error humanization).
It is the working to-do list for the next iteration.

Fix priorities: P0 = correctness/security, P1 = robustness/polish, P2 = delight.
Line references are to this repo's current code.

---

## P0 - Engine correctness & security gaps (fix first)

1. **Ranged workers have no per-chunk byte cap.** A non-conforming or malicious
   server can answer a 206 with a body longer than its declared Content-Range; the
   extra bytes spill past the chunk end and can overwrite an *already completed*
   neighboring chunk - and the completion length check still passes (the .part is
   preallocated). The single-stream path already has `trim_to_target`
   (src-tauri/src/engine/download.rs:256-263); the ranged path does not
   (download.rs:395-437). **Adopt from hydra-pi**: its `allowed`/`room` logic caps
   every read to the remaining chunk bytes (hydra-pi engine.rs:512-521).

2. **Unknown-size downloads can ship a stale .part tail.** Fresh plans open the
   .part with `truncate(false)` (download.rs:193-195) and `set_len` only runs when
   the size is known (download.rs:944-951), while finalize accepts any length when
   size is null (download.rs:876). Result: an unknown-size download that reuses a
   filename with an older, larger leftover .part silently renames new content plus
   a stale tail as "complete". Fix: truncate to 0 when starting a fresh
   unknown-size plan.

3. **The completion "size verify" is tautological.** The .part is preallocated via
   `set_len(size)`, so `metadata().len() == size` (download.rs:857-868) can never
   fail in the normal flow; the meta clamp check (meta.rs:90-99) is likewise a
   no-op unless the file was damaged externally. Real correctness rests entirely
   on done-counters-advancing-only-after-successful-writes (sound, but the
   advertised safety net is illusory). Fix: verify something real - e.g. per-chunk
   byte accounting against each response's declared Content-Range length - and
   document the actual guarantee. (Note: this build is already the only one of
   the three that fsyncs before rename - keep that.)

4. **No mid-body read/idle timeout.** The shared client sets a connect timeout
   only (engine/mod.rs). A silently black-holed flow stalls its worker forever:
   no error, no retry, the UI shows speed dropping to zero with no explanation -
   and the only escape (cancel) deletes all progress. This is the single most
   probable real-world failure on exactly the bandwidth-unstable network this
   app exists for. (All three sibling builds share this flaw.) Fix: wrap each
   block read in `tokio::time::timeout` (e.g. 30 s) and treat a stall like any
   retryable stream error.

5. **Blocking file I/O on the async runtime - no spawn_blocking anywhere.** The
   128 KiB `write_all_at` per block (download.rs:238), the per-second atomic meta
   saves (download.rs:697-705, meta.rs:35-46), and - worst - finalize's
   `sync_all()` of a fully dirty multi-GiB file (download.rs:851) all run on
   runtime worker threads. On APFS with a warm page cache this is sub-second, but
   on slow disks it can stall the runtime and hitch progress events.
   **Adopt from hydra-pi**: it wraps its pwrite in `spawn_blocking`
   (hydra-pi engine.rs:423-424) - the concept is right. (Port the concept, not
   the bug: hydra-pi's actual call was one-shot `write_at` discarding the byte
   count, which is precisely how a partial disk-full write corrupts silently.
   Keep `write_all_at` semantics inside the `spawn_blocking` wrapper.)

6. **Webview hardening: `csp: null` + `withGlobalTauri: true`** exposes the full
   invoke surface with no Content-Security-Policy (tauri.conf.json:29). Local-only
   content mitigates, but a pinned CSP is standard Tauri hardening. **Adopt from
   hydra-pi**: it ships a hand-authored meta CSP (hydra-pi index.html:6) - take
   that as a starting point and set it properly in tauri.conf.json.

7. **Secret hygiene in the published repo (done, keep it that way).** The
   reference CDN URL with its live token was scrubbed from the tree and from all
   git history before publishing; acceptance.sh now takes the target from
   `HYDRA_REF_URL` and the CDN stages SKIP when it is unset. Do not reintroduce
   credential-bearing URLs into the repo.

## P1 - Robustness, resources & polish

8. **Probe trusts `Accept-Ranges` on the HEAD path.** If HEAD answers 200 without
   the header, this build marks ranges unsupported and goes single-stream - even
   though many servers support ranges but omit the header on HEAD. **Adopt from
   hydra-pi (and hydra-deepseek, which does the same)**: an active
   `Range: bytes=0-0` verification GET when the header is absent
   (hydra-pi probe.rs:112-118). Both sibling builds do this; it is strictly more
   correct and it can upgrade a would-be single-stream download to full speed.

9. **meta_check builds sidecar paths from an unsanitized filename**
   (bridge.rs:244-246) while start_download sanitizes (bridge.rs:116). Read-only
   today, but mirror the sanitize for consistency.

10. **Unbounded mpsc channels** for events/worker-msgs/ctrl (download.rs:135,
    download.rs:747, bridge.rs:136). The bridge forwarder drains fast, so growth
    is theoretical - but a stalled webview consumer has no backpressure. Consider
    a bounded progress channel with drop-oldest semantics, or document the
    design choice.

11. **Error-string hygiene**: `"io error: {}"` (download.rs:206) and
    `"could not stat partial file: {}"` (download.rs:872) leak internal text to
    the user. **Adopt from hydra-deepseek**: its `humanize_error` map
    (hydra-deepseek model.rs:98-114) converts disk-full / permission / DNS /
    connection-refused errno text into friendly strings. Blend that with the
    existing contract error list.

12. **Filename sanitizer should strip RTL-override characters** (U+202E/U+202D),
    which can visually spoof filenames. **Adopt from hydra-deepseek**
    (hydra-deepseek probe.rs:19-31) - it is the only build of the three that
    blocks bidi-override spoofing.

13. **Missing test coverage at the seams.** The async core (worker loops, monitor,
    finalize, supervisor transitions) has no cargo-level test - it is exercised
    only via the speedtest + acceptance harness. Add acceptance stages for:
    cancel (asserts .part + meta deletion), unknown-size downloads, mid-download
    range refusal, and meta corruption/truncation. Also: an over-sending 206
    server variant to pin fix #1 (testserver.py already makes this easy).

14. **Capability narrowing**: `dialog:default` is broader than needed - only
    folder-open is used; `dialog:allow-open` would be least-privilege.

15. Minor cleanups: dead `hydra_ping` command (bridge.rs:90-93, lib.rs:15);
    unused tokio `io-util` feature (Cargo.toml:19); `save_settings` casts
    u64->u32 before clamping (bridge.rs:231); the stranded doc comment above the
    bridge tests block (bridge.rs:296-298).

## UI changes to adopt - from hydra-pi (the focus of this roadmap)

hydra-pi's frontend is the most accessible and best-hardened of the three builds.
These ports fit this repo's existing token system (ui/ui.css) one-for-one:

1. **Progressbar semantics on the overall bar.** `role="progressbar"` with
   `aria-valuemin/max/now` plus `aria-valuetext` ("1.2 GiB of 7.24 GiB") updated
   per tick (hydra-pi index.html:112-122, main.js:347-348). This repo's bar is
   visual-only today - the single most valuable a11y port.
2. **Explicit `:focus-visible` outlines** (hydra-pi styles.css:619-622). This
   repo deliberately trades input focus rings for border+glow; keep the glow but
   add keyboard-visible outlines so focus is never invisible.
3. **`<output>` element for the connection stepper value** (hydra-pi
   index.html:63) - semantic, accessible association between slider and value.
4. **Status microcopy map** - "Paused - offsets saved", "Streaming N
   connections", "Checking the origin..." (hydra-pi main.js:419-428) as a muted
   status line under the primary action; pairs well with the existing notes.
5. **Inline done-panel inside the progress card**: check + final path + "Reveal
   in Finder" right where the progress was (hydra-pi index.html:130-139) - keeps
   the completion moment in context instead of jumping layout.
6. **Per-connection summary line** ("3/8 complete") above the comb grid
   (hydra-pi index.html:124-128) - glanceable state without reading 16 bars.
7. **Gradient progress fill** (accent-deep -> accent -> mint, hydra-pi
   styles.css:489) - livelier than the flat fill while staying one-accent
   compliant if mint is treated as an accent tint.
8. **Responsive layout pass for narrow windows** (@media max-width, hydra-pi
   styles.css:624) - the window is resizable; the comb grid and stat rows
   cramp below ~700px today.
9. **"Engine ready" badge** after backend init (hydra-pi index.html:24,
   main.js:508) - a small perceived-responsiveness touch.
10. **Page-level CSP as defense-in-depth** (hydra-pi index.html:6) - in addition
    to the real fix at the config level (P0-6).

What NOT to copy from hydra-pi: its URL field lacks a monospace font (this repo
already does that better), and its shipped placeholder contained a live
tokenized CDN URL (secret-leak anti-pattern - this repo's mock stays neutral).

## Smaller picks from hydra-deepseek (visual character)

hydra-deepseek is the most visually distinctive build; these slot into this
repo's header without breaking the one-accent rule:

- **Status pill with a live dot** in the titlebar (hydra-deepseek styles.css:64-72)
  - idle/download/error states at a glance.
- **The 3-bar brand mark** with staggered heights (styles.css:52-60) - echoes the
  multi-stream idea in 30 px.
- **Numbered section steps (01 / 02 / 03)** (index.html:28,45,77) - subtle
  wayfinding through Source -> Details -> Destination.
- **Custom slider with a --fill track** (styles.css:181-190) - shows the current
  position in the track itself.
- **Optimistic Pause/Resume with rollback** on command failure
  (main.js:270-284) - snappier than await-then-render; this repo's
  `S.starting` flag already guards the race, so the optimistic layer is a pure
  responsiveness win.

## Accepted trade-offs (documented - no action)

- Static chunk assignment (worker i owns chunk i for the whole run) -
  spec-faithful and matches the per-connection bars.
- `Overlay` + `hiddenTitle` instead of a literal "hiddenInset" titlebar style -
  Tauri v2 has no HiddenInset value; the combination is the supported equivalent
  (documented in CONTRACT.md).
- The mock harness ships inside the frontend bundle (~2 KB, self-disables under
  real Tauri) - acceptable; split it out if the bundle ever grows.
- fsync-before-rename at completion - already unique among the three builds; keep.

## Verification plan for the fixes

| Fix | How to prove it |
|---|---|
| #1 byte cap | testserver.py variant that over-sends range bodies; assert sha1 of the file |
| #2 unknown-size truncate | local no-Content-Length server stage; assert file == exact body |
| #4 read timeout | testserver.py "silent stall" mode; assert retry fires within N seconds |
| #5 spawn_blocking writes | keep the 4 Hz event cadence during a large finalize (timing assert in speedtest) |
| #6 CSP | run the app; console shows no CSP violations across all mock states |
| #8 active range verification | HEAD-405-then-ranges stage already exists; add a no-Accept-Ranges-header variant |
| #11/#12 error + sanitize | unit tests (pure functions, trivial to pin) |

## Non-goals (unchanged)

Torrents/magnet links, browser extension, scheduling/queues, accounts,
telemetry.

---

## Status after the second audit round

A second, independent read-only audit of this repo (25 correctness/contract findings + 8
resource findings) was cross-checked line by line against the code. All of it was real; two
items were judgement calls and are answered below. Everything was then fixed on
`fix/audit-round-2`.

| Finding | State |
|---|---|
| #1 cancel returned before cleanup | **fixed** - `Ctrl::Cancel` carries a oneshot ack; the bridge replies only once the files are gone |
| #2 ranged worker dropped its buffered tail on EOF/stall/error | **fixed** - the buffer is flushed before every retry (this was a regression from the previous round) |
| #3 resumed run + HTTP 200 truncated the .part | **fixed** - the fallback arm now requires `!plan.resumed` |
| #4 start ack could resurrect a settled run | **fixed** - the ack is ignored once the run is no longer starting; terminal events end the handshake |
| #5 resume stage hashed a stale file (vacuous proof) | **fixed** - the wipe step deletes the completed file too, so the resumed bytes are what gets hashed |
| #6 sidecar fsynced before the .part | **fixed** - the .part is fsynced inside the same blocking save, before the sidecar claims it |
| #7 pause claimed "offsets saved" after a failed flush | **fixed** - `flush_meta` returns `io::Result` and the pause path reports an error instead |
| #8 stall stage passed without a read deadline | **fixed** - the assertion is now two-sided (28-42 s) |
| #9 ranged stages never asserted `single=0` | **fixed** |
| #10 a late probe result could override an edited URL | **fixed** - probe generation counter plus a URL match check |
| #11 / #25 / R6 fixed scratch dir | **fixed** - `mktemp -d`, removed on exit, never reused |
| #12 `exists()` then `rename` TOCTOU | **fixed** - the target is reserved with `create_new`; a contention test pins it |
| #13 terminal event after cancel | **fixed** - retired ids are remembered and their events dropped |
| #14 `abort()` cannot stop in-flight `spawn_blocking` | **fixed** - saves check a dedicated cancelling flag before renaming, plus the existing sweep |
| #15 no coverage for range verification or `size=null` | **fixed** - new `noadvertise` / `nolength` server modes and stages 9-10 |
| #16 paused note outlived its state | **fixed** |
| #17 reveal error class leaked into the next completion | **fixed** |
| #18 / R5 `.hydra-write-probe` could truncate a user file | **fixed** - unique name reserved with `create_new` |
| #19 server header could come from HEAD while the GET decided | **fixed** - only the deciding response is reported |
| #20 raw error text reached the UI | **fixed** |
| #21 concurrent settings saves could drop a field | **fixed** - serialized behind a lock |
| #22 stale docs | **fixed** - README, CONTRACT and scripts/README-TESTS.md describe the shipped behaviour |
| #23 docstring named a flag that did not exist | **fixed** - `--stall-prefix` is now a real flag |
| #24 oversend could not over-send the final chunk | **fixed** - the filler wraps around past EOF |
| R1 unbounded event queue | **fixed** - bounded channel; progress dropped when full, state waits briefly so done/error are never lost |
| R2 no cap on concurrent downloads | **fixed** - 8-run cap plus a per-`.part` ownership check |
| R8 a new HTTP client per probe/run | **fixed** - one process-wide client (90 s idle pool) |

### Deliberate non-changes

- **R3** - a failed run still keeps its `.part` and sidecar. That is the input to "Try again" and
  to a later resume, and it can be gigabytes the user already paid for. Cancel is the explicit
  delete action, and it now acks only after the files are gone. Documented in CONTRACT.md.
- **R4** - the sidecar temp name stays deterministic because cancel cleanup has to name it to
  delete it; unique temps would make a crashed save's leftover unsweepable. Concurrent saves of
  one path are impossible (single writer, joined before the pause flush) and a leftover is
  overwritten by the next save. Documented in `meta::tmp_path`.
