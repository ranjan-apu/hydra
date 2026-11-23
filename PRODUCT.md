# Hydra - Product Truth

> Provenance: inferred by the ui agent from the project brief (the impeccable init interview
> is not interactive at agent depth), reviewed and persisted here by lead, who owns the repo
> root. Update this file when the product changes - it is the reference for all UI work.

## Product

- **Platform**: native macOS app - Tauri v2 shell (Rust backend + system WKWebView, no bundled
  Chromium), macOS-dark chrome (Overlay titlebar, hiddenTitle, inset traffic lights).
- **Stack**: vanilla ES2020 frontend (no frameworks, no build step); Rust engine (tokio,
  reqwest/rustls) under the same contract (CONTRACT.md).
- **Users**: people on a network where a single connection swings 2.8-29.4 MiB/s while 4
  parallel flows sustain ~26 MiB/s, pulling multi-GiB files from HTTP/1.1 origins.
- **Purpose**: make multi-connection downloading visible and effortless - paste URL, probe,
  pick folder, download; live speed, ETA, and per-connection fill bars are the hero elements.
- **Positioning**: parallel multi-stream downloader with per-connection visibility and
  automatic sidecar resume, in a native macOS shell.
- **Capabilities**: probe (HEAD -> ranged-GET fallback, plus an active range verification
  when the origin omits Accept-Ranges), editable filename, remembered folder + 1-16
  connections, parallel ranged download with per-chunk retry and per-chunk byte caps,
  pause/resume/cancel, .part + .hydra.json sidecar resume across restarts, single-stream
  fallback when ranges are refused or size unknown, per-chunk byte accounting at completion,
  Reveal in Finder.
- **What the probe surfaces** (this is product value, not chrome): size, whether ranges are
  supported, which method decided it (HEAD / Range GET), the answering server, and whether a
  resumable partial already exists - all before the user commits to a download.
- **Brand commitments**: "Hydra" wordmark over a three-stream brand mark that answers live
  transfer; a single spring-mint accent (#4fe0b0) on night-blue graphite; monospace for all
  data (URLs, names, paths, byte counts); English copy; no emoji - hand-drawn inline SVGs only.

## Principles

1. The multi-connection advantage must be visible (the conn grid is the hero).
2. Never leave the user uninformed - backend-driven speed/ETA at >= 4 Hz.
3. Native restraint - one accent doing real work, tonal depth, shadows only as soft
   offset plates under raised surfaces.
4. The backend is the source of truth for speed/ETA; the UI only formats.
5. Errors are human lowercase strings, shown verbatim.
