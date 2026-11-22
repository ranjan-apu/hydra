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
- **Capabilities**: probe (HEAD -> ranged-GET fallback), editable filename, remembered folder
  + 1-16 connections, parallel ranged download with per-chunk retry, pause/resume/cancel,
  .part + .hydra.json sidecar resume across restarts, single-stream fallback when ranges are
  refused or size unknown, final size verify, Reveal in Finder.
- **Brand commitments**: "HYDRA" letterspaced wordmark; pinned palette/type/radius; English
  copy; no emoji - hand-drawn inline SVGs only.

## Principles

1. The multi-connection advantage must be visible (the conn grid is the hero).
2. Never leave the user uninformed - backend-driven speed/ETA at >= 4 Hz.
3. Native restraint - one accent, tonal depth, no shadows.
4. The backend is the source of truth for speed/ETA; the UI only formats.
5. Errors are human lowercase strings, shown verbatim.
