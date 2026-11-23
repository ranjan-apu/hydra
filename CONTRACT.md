# Hydra - Integration Contract (canonical interface)

Hydra: native macOS multi-stream downloader - Tauri v2 (Rust backend + WKWebView), vanilla JS
frontend, tokio + reqwest(rustls) engine. This file is the single source of truth for every
cross-boundary interface. The engine agent implements the Rust side, the ui agent the JS side,
the testlab agent the local acceptance harness. Deviations from this contract are bugs.

## Ownership

| Owner | Paths |
|---|---|
| lead (orchestrator) | package.json, .gitignore, src-tauri/Cargo.toml, src-tauri/tauri.conf.json, src-tauri/capabilities/, src-tauri/build.rs, icons/, assets/, README.md, git, builds, acceptance runs |
| engine agent | src-tauri/src/, src-tauri/examples/ |
| ui agent | ui/ |
| testlab agent | scripts/ |

Never edit a file you do not own. Exception: the engine agent may append entries under
[dependencies] in src-tauri/Cargo.toml (never remove or rename existing ones). Contract
changes go through lead.

## Wire conventions

- Single window, label "main". withGlobalTauri: true (window chrome: Overlay titlebar,
  hiddenTitle, traffic lights top-left, dark theme - already configured).
- Frontend invokes: await window.__TAURI__.core.invoke(name, argsObject) - arg keys camelCase
  exactly as written below.
- Events: const unlisten = await window.__TAURI__.event.listen(name, cb) - payload at cb.payload.
- Rust: every wire-facing serde type uses #[serde(rename_all = "camelCase")].
- Human error strings are lowercase and shown verbatim by the UI.

## Commands

### probe_url - args { url }
Returns ProbeInfo:

    { "url": "https://final-after-redirects", "size": 7772412965, "acceptsRanges": true,
      "filename": "resolved-name.ext", "contentType": "application/octet-stream",
      "probeMethod": "head", "server": "nginx/1.25.3" }

size is null when unknown. Err(string) on failure: "could not connect to host", "HTTP 403", ...
server is the raw `Server` response header of the response that decided the result; the key is
omitted entirely when the origin sent none (it is the only additive field in this payload).

Behavior: HEAD first; on 403/405/501 fall back to a ranged GET with header "Range: bytes=0-0".
- Ranged GET -> 206 + Content-Range "bytes 0-0/TOTAL": acceptsRanges=true, size=TOTAL,
  probeMethod="range".
- HEAD 200 WITH Accept-Ranges containing "bytes": acceptsRanges=true,
  size=Content-Length (null if absent), probeMethod="head".
- HEAD 200 WITHOUT Accept-Ranges: the header is advisory and many origins omit it while
  honouring Range. One verification GET ("Range: bytes=0-0") decides: a 206 means
  acceptsRanges=true (total from Content-Range when HEAD gave no length), a 200 means
  acceptsRanges=false. probeMethod="range" when that GET decided. If the verification GET
  itself fails, the HEAD conclusion stands - verification is an optimisation, never a
  precondition.
- Ranged GET -> 200 (Range ignored): acceptsRanges=false, size=Content-Length (null if absent),
  probeMethod="range".

Only headers are read for any range verification: the body is dropped unread, so validating
range support never pulls the file.

### start_download - args { url, destDir, filename, connections, size, acceptsRanges }
url is the FINAL resolved URL from probe (workers should not re-follow redirects).
connections is 1..16. size may be null. Returns { "id": "dl-1" }.

Semantics:
- Sidecar resume: if <destDir>/<filename>.hydra.json and <destDir>/<filename>.part exist, and
  meta.url == url and meta.size == size (when size is non-null), resume from the saved chunk
  offsets; the first hydra://state event carries note "Resuming partial download".
- Fresh: chunkSize = ceil(size / connections); chunk i (0-based): start = i*chunkSize,
  end = min((i+1)*chunkSize - 1, size - 1). Static assignment: connection i owns chunk i for
  the whole run (per-connection bars reflect exactly this).
- acceptsRanges=false or size=null -> single-stream mode (one connection, sequential from 0,
  no chunking). size=null additionally means: no meta sidecar, no resume.
- If a ranged download receives HTTP 200 before any byte was written -> automatic
  single-stream fallback, note "server refused range requests - using a single connection".
Err strings: "destination is not writable: <path>", "disk full", ...
Emits hydra://state "downloading" (with note when resuming or falling back), then
hydra://progress at >= 4 Hz.

### pause_download - args { id } -> null
Stops all workers, flushes meta, emits state "paused". Keeps .part + meta + in-memory handle.

### resume_download - args { id } -> null
Emits state "downloading".

### cancel_download - args { id } -> null
Stops workers, DELETES <filename>.part and <filename>.hydra.json, forgets the download.
No state event afterwards.

### pick_folder - args {} -> absolute folder path string | null
Native folder picker (opens at the remembered folder when set).

### reveal_path - args { path } -> null
Finder reveal of the item.

### get_settings - args {} -> { "destDir": string | null, "connections": 8 }

### save_settings - args { destDir?: string | null, connections?: number } -> null
Partial update; persisted at <appConfigDir>/settings.json.

### meta_check - args { url, destDir, filename, size }
Returns { "exists": bool, "compatible": bool, "bytesDone": number } - whether a resumable
sidecar matching url+size exists.

## Events

### hydra://state - payload

    { "id": "dl-1", "state": "downloading" | "paused" | "done" | "error",
      "path": "/final/file.mkv",        // state=done only
      "size": 7772412965,               // state=done only (verified byte count)
      "message": "HTTP 403",            // state=error only
      "note": "Resuming partial download" }  // optional, any state

### hydra://progress - payload (every ~250 ms while downloading; final one before "done" reflects complete totals)

    { "id": "dl-1", "bytesDone": 1234567, "total": 7772412965,
      "speedBps": 27000000, "etaSecs": 245,
      "conns": [ { "index": 0, "done": 1234567, "size": 1943103242, "active": true,
                   "phase": "idle" | "connecting" | "receiving" | "retrying" | "done" } ] }

total is null when unknown; etaSecs is null when total unknown or speed ~ 0.
speedBps is a rolling ~2 s window. conns length = active connection count (1 in single-stream).

## Sidecar meta <destDir>/<filename>.hydra.json - EXACT schema

    { "url": "https://...", "size": 7772412965,
      "chunkSize": 1943103242,
      "chunks": [ { "start": 0, "end": 1943103241, "done": 1943103242 } ] }

done = bytes of that chunk already safely on disk. Partial data file:
<destDir>/<filename>.part, positional writes only (pwrite at absolute offsets, one file handle
per worker). Flush at most 1/s when dirty, and immediately on pause. On completion: fsync ->
verify size -> rename .part to <destDir>/<filename> (existing target -> "<stem> (k)<ext>" for
k=2,3,...) -> delete meta -> emit state "done" with path + size.

## Error strings (backend -> UI, verbatim, human, lowercase)

"server refused range requests - using a single connection" (note)
"server refused range requests mid-download"
"HTTP 403" / "HTTP 404" (any "HTTP <code>")
"could not connect to host"
"connection lost mid-download (after 5 retries)"
"disk full"
"destination is not writable: <path>"
"the destination folder disappeared"
"could not write to disk"
"server returned an unexpected Content-Range"
"downloaded size mismatch: expected <n>, got <m>"
"empty response from server"

## Frontend state machine

idle -> probing -> ready -> downloading -> paused -> done | error
(error -> ready via "Try again" with the same parameters). The UI never computes speed or ETA
(backend supplies both); it formats: IEC bytes (GiB/MiB), speed MiB/s with 1 decimal,
ETA "4m 12s" / "1h 03m".

## Headless speedtest harness (engine implements; testlab scripts consume)

    src-tauri/target/release/examples/speedtest <url> [conns=4] [max_bytes=1073741824] [out=/tmp/hydra-speed.part] [run_seconds=0] [cancel_after_secs=0]

- Probes, then downloads the FIRST max_bytes bytes of <url> using the same engine core as the
  app, with conns static chunks over [0, max_bytes).
- run_seconds > 0: after that many seconds of ACTIVE downloading, pause exactly like the app
  (flush sidecar at <out minus .part>.hydra.json, keep .part), print "PAUSED bytes=<n>", exit 0.
  A later run with the same url/out resumes from the meta (prints "RESUMED bytes=<n>").
- cancel_after_secs > 0: after that many seconds of ACTIVE downloading, cancel exactly like the
  app - stop the workers, then delete <out> and <out minus .part>.hydra.json (plus any
  .hydra.json.tmp left by an in-flight save). Prints "CANCELLED bytes=<n>" and a SUMMARY with
  cancelled=1, exit 0. Nothing may be left behind on disk.
- Prints one "TICK t=<s> done=<bytes> speed_mibps=<x.x>" line per second.
- Final machine-readable line: "SUMMARY ok=1 bytes=<n> elapsed_s=<x.x> avg_mibps=<y.y>
  resumed=<0|1> single=<0|1>" or "SUMMARY ok=0 error=<message>"; exit code 0 iff ok=1.
  avg_mibps is an honest wall-clock average of the transfer (probing excluded).

## Reference test target (NOT committed to this repo)

No specific download target is committed here. The acceptance harness takes its
reference URL from the environment:

    export HYDRA_REF_URL="https://your-cdn.example/file.bin?token=..."

A good reference target: HTTP/1.1, Accept-Ranges: bytes, ranged GET returning
206 with a Content-Range total, a Content-Length of at least ~1 GiB, on a
network where a single flow is bandwidth-unstable (the motivating pathology).
Acceptance: pulling it with 4 connections must average >= 25 MiB/s.
Without HYDRA_REF_URL the two CDN stages SKIP and the local + unit stages run.
