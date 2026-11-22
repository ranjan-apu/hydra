/* main.js - Hydra frontend.
 *
 * Talks ONLY to window.__TAURI__ (Tauri v2, withGlobalTauri):
 *   await window.__TAURI__.core.invoke(name, args)   - camelCase arg keys per CONTRACT.md
 *   await window.__TAURI__.event.listen(name, cb)    - payload at cb.payload
 *
 * State machine: idle -> probing -> ready -> downloading -> paused -> done | error
 * (error -> ready via "Try again" with the same parameters).
 * Speed and ETA always come from the backend (hydra://progress); the UI only
 * formats IEC bytes, MiB/s and "4m 12s left" / "1h 03m left".
 */

"use strict";

(function () {
  var $ = function (id) { return document.getElementById(id); };

  var els = {
    urlInput: $("url-input"),
    urlMsg: $("url-msg"),
    urlMsgText: $("url-msg-text"),
    readyCard: $("ready-card"),
    filenameInput: $("filename-input"),
    sizeText: $("size-text"),
    rangesBadge: $("ranges-badge"),
    typeChip: $("type-chip"),
    resumeBadge: $("resume-badge"),
    saveControls: $("save-controls"),
    destText: $("dest-text"),
    chooseBtn: $("choose-btn"),
    connMinus: $("conn-minus"),
    connPlus: $("conn-plus"),
    connValue: $("conn-value"),
    connNote: $("conn-note"),
    downloadBtn: $("download-btn"),
    folderPrompt: $("folder-prompt"),
    progressCard: $("progress-card"),
    dlName: $("dl-name"),
    speedReadout: $("speed-readout"),
    barTrack: $("bar-track"),
    barFill: $("bar-fill"),
    statusLine: document.querySelector(".status-line"),
    stBytes: $("st-bytes"),
    stSpeed: $("st-speed"),
    stEta: $("st-eta"),
    stateNote: $("state-note"),
    connGrid: $("conn-grid"),
    errorBanner: $("error-banner"),
    errorMsg: $("error-msg"),
    doneBlock: $("done-block"),
    doneSize: $("done-size"),
    donePath: $("done-path"),
    pauseBtn: $("pause-btn"),
    resumeBtn: $("resume-btn"),
    cancelBtn: $("cancel-btn"),
    retryBtn: $("retry-btn"),
    revealBtn: $("reveal-btn")
  };

  var MIN_CONNECTIONS = 1;
  var MAX_CONNECTIONS = 16;

  var S = {
    state: "idle",            // idle | probing | ready | downloading | paused | done | error
    settings: { destDir: null, connections: 8 },
    probe: null,               // last successful ProbeInfo
    stepperLocked: false,      // single-stream server: connections are not applicable
    dlId: null,
    starting: false,           // bridges start_download's invoke/event race
    finalPath: null,
    lastProgress: null,
    rafPending: false,
    connCells: [],             // grid is built once per download, then mutated
    gridFor: -1,
    metaSeq: 0
  };

  function invoke(name, args) {
    return window.__TAURI__.core.invoke(name, args);
  }

  function errText(err) {
    if (typeof err === "string") return err;
    if (err && typeof err.message === "string") return err.message;
    return String(err);
  }

  // ---------- formatting (the UI never computes speed or ETA) ----------

  function formatBytes(n) {
    if (n == null || !isFinite(n)) return "Unknown size";
    n = Number(n);
    if (n < 1024) return Math.round(n) + " B";
    var units = ["KiB", "MiB", "GiB", "TiB"];
    var v = n;
    var u = -1;
    do { v /= 1024; u++; } while (v >= 1024 && u < units.length - 1);
    var digits = v >= 100 ? 0 : v >= 10 ? 1 : 2;
    return v.toFixed(digits) + " " + units[u];
  }

  function formatSpeed(bps) {
    if (bps == null || !isFinite(bps) || bps < 0) return null;
    return (bps / 1048576).toFixed(1) + " MiB/s";
  }

  function formatEtaLeft(secs) {
    if (secs == null || !isFinite(secs) || secs < 0) return null;
    secs = Math.round(secs);
    if (secs < 60) return secs + "s left";
    if (secs < 3600) {
      var m = Math.floor(secs / 60);
      var s = secs % 60;
      return m + "m " + String(s).padStart(2, "0") + "s left";
    }
    var h = Math.floor(secs / 3600);
    var mm = Math.floor((secs % 3600) / 60);
    return h + "h " + String(mm).padStart(2, "0") + "m left";
  }

  function middleEllipsis(text, max) {
    text = String(text);
    if (text.length <= max) return text;
    var head = Math.ceil((max - 1) / 2);
    var tail = Math.floor((max - 1) / 2);
    return text.slice(0, head) + "\u2026" + text.slice(text.length - tail);
  }

  function pct(done, total) {
    if (total == null || !isFinite(total) || total <= 0) return null;
    var d = Number(done);
    if (!isFinite(d) || d < 0) d = 0;
    return Math.min(100, (d / total) * 100);
  }

  function clampConnections(n) {
    var v = Math.round(Number(n));
    if (!isFinite(v)) return 8;
    return Math.min(MAX_CONNECTIONS, Math.max(MIN_CONNECTIONS, v));
  }

  // ---------- state machine ----------

  function setState(next) {
    S.state = next;
    document.body.dataset.state = next;
    render();
  }

  function render() {
    var st = S.state;
    els.readyCard.hidden = st !== "ready";

    var inProgress = st === "downloading" || st === "paused" || st === "done" || st === "error";
    els.progressCard.hidden = !inProgress;

    // the transfer surface stays visible while a run is live, failed, or paused;
    // "done" replaces it with the completion block
    var live = st === "downloading" || st === "paused" || st === "error";
    els.barTrack.hidden = !live;
    if (els.statusLine) els.statusLine.hidden = !live;
    els.connGrid.hidden = !live;
    els.doneBlock.hidden = st !== "done";
    els.errorBanner.hidden = st !== "error";
    if (st === "done") els.stateNote.hidden = true;

    els.pauseBtn.hidden = st !== "downloading";
    els.resumeBtn.hidden = st !== "paused";
    els.cancelBtn.hidden = !(st === "downloading" || st === "paused");
    els.retryBtn.hidden = st !== "error";
    els.revealBtn.hidden = st !== "done";

    els.pauseBtn.disabled = false;
    els.resumeBtn.disabled = false;
    els.cancelBtn.disabled = false;
  }

  // ---------- probing ----------

  function maybeProbe() {
    if (S.starting) return;
    if (S.state === "probing" || S.state === "downloading" || S.state === "paused") return;
    var url = els.urlInput.value.trim();
    if (!url) return;
    doProbe(url);
  }

  async function doProbe(url) {
    S.starting = false;
    S.dlId = null;
    setState("probing");
    els.urlMsg.classList.remove("is-error");
    els.urlMsgText.textContent = "Probing server...";
    try {
      var info = await invoke("probe_url", { url: url });
      S.probe = info;
      S.metaSeq++; // any in-flight meta_check is now stale
      populateReady(info);
      els.urlMsgText.textContent = "";
      setState("ready");
      if (S.settings.destDir) metaCheck();
    } catch (err) {
      els.urlMsg.classList.add("is-error");
      els.urlMsgText.textContent = errText(err);
      setState("idle");
    }
  }

  // ---------- ready card ----------

  function currentFilename() {
    var v = els.filenameInput.value.trim();
    if (v) return v;
    return S.probe ? S.probe.filename : "";
  }

  function populateReady(info) {
    els.filenameInput.value = info.filename;
    els.sizeText.textContent = formatBytes(info.size);
    els.sizeText.classList.toggle("is-unknown", info.size == null);

    var multi = !!info.acceptsRanges && info.size != null;
    els.rangesBadge.textContent = multi ? "Ranges - multi-connection" : "Single stream only";
    els.rangesBadge.className = "badge " + (multi ? "badge-green" : "badge-amber");
    els.typeChip.textContent = info.contentType || "";
    els.typeChip.hidden = !info.contentType;

    els.resumeBadge.hidden = true;

    S.stepperLocked = !multi;
    els.connNote.hidden = multi;
    els.connNote.textContent = multi
      ? ""
      : (info.size == null ? "unknown file size - one connection" : "server ignores range requests");
    refreshStepper();

    renderDestRow();
    showFolderPrompt(null, false);
    els.folderPrompt.hidden = true;
    updateDownloadButton();
  }

  function renderDestRow() {
    if (S.settings.destDir) {
      els.destText.textContent = middleEllipsis(S.settings.destDir, 46);
      els.destText.classList.remove("is-empty");
    } else {
      els.destText.textContent = "No folder chosen";
      els.destText.classList.add("is-empty");
    }
  }

  function updateDownloadButton() {
    var noFolder = !S.settings.destDir;
    els.downloadBtn.classList.toggle("is-disabled", noFolder);
    if (noFolder) els.downloadBtn.setAttribute("aria-disabled", "true");
    else els.downloadBtn.removeAttribute("aria-disabled");
  }

  function showFolderPrompt(text, isError) {
    els.folderPrompt.hidden = false;
    els.folderPrompt.textContent = text || "Choose a folder to continue";
    els.folderPrompt.classList.toggle("is-error", !!isError);
  }

  var attnTimer = 0;
  function attnSaveRow() {
    els.saveControls.classList.remove("attn");
    // restart the flash so a second click re-pulses
    void els.saveControls.offsetWidth;
    els.saveControls.classList.add("attn");
    clearTimeout(attnTimer);
    attnTimer = setTimeout(function () { els.saveControls.classList.remove("attn"); }, 1200);
  }

  function refreshStepper() {
    var n = S.settings.connections;
    els.connValue.textContent = String(n);
    els.connMinus.disabled = S.stepperLocked || n <= MIN_CONNECTIONS;
    els.connPlus.disabled = S.stepperLocked || n >= MAX_CONNECTIONS;
  }

  function stepConnections(delta) {
    if (S.stepperLocked) return;
    var n = clampConnections(S.settings.connections + delta);
    if (n === S.settings.connections) return;
    S.settings.connections = n;
    refreshStepper();
    invoke("save_settings", { connections: n }).catch(function () { /* settings persist best-effort */ });
  }

  async function metaCheck() {
    if (!S.probe || !S.settings.destDir) { els.resumeBadge.hidden = true; return; }
    var seq = ++S.metaSeq;
    try {
      var res = await invoke("meta_check", {
        url: S.probe.url,
        destDir: S.settings.destDir,
        filename: currentFilename(),
        size: S.probe.size
      });
      if (seq !== S.metaSeq || S.state !== "ready") return; // stale
      var p = pct(res.bytesDone, S.probe.size);
      var show = !!res.compatible && res.bytesDone > 0 && p != null && p > 0;
      els.resumeBadge.hidden = !show;
      if (show) {
        els.resumeBadge.textContent = "Partial download found - will resume from " + p.toFixed(1) + "%";
      }
    } catch (err) {
      if (seq === S.metaSeq) els.resumeBadge.hidden = true;
    }
  }

  // ---------- download lifecycle ----------

  function resetProgressSurface() {
    els.barFill.style.width = "0%";
    els.barTrack.classList.remove("indeterminate");
    els.speedReadout.textContent = "";
    els.stBytes.textContent = "";
    els.stSpeed.textContent = "";
    els.stEta.textContent = "";
    els.stateNote.hidden = true;
    els.stateNote.textContent = "";
    els.errorMsg.textContent = "";
    els.connGrid.textContent = "";
    els.connGrid.classList.remove("single");
    S.connCells = [];
    S.gridFor = -1;
  }

  async function startDownload() {
    if (!S.probe || S.starting || !S.settings.destDir) return;
    var args = {
      url: S.probe.url,           // final resolved URL from probe
      destDir: S.settings.destDir,
      filename: currentFilename(),
      connections: S.settings.connections,
      size: S.probe.size,
      acceptsRanges: S.probe.acceptsRanges
    };
    S.starting = true;
    S.lastProgress = null;
    S.finalPath = null;
    els.dlName.textContent = args.filename;
    els.dlName.title = args.filename;
    resetProgressSurface();
    try {
      var res = await invoke("start_download", args);
      if (S.starting && res && res.id) S.dlId = res.id;
      setState("downloading");
    } catch (err) {
      S.starting = false;
      showError(errText(err));
    }
  }

  function showError(message) {
    els.errorMsg.textContent = message;
    setState("error");
  }

  function showNote(text) {
    els.stateNote.textContent = text;
    els.stateNote.hidden = false;
  }

  // ---------- backend events ----------

  function onState(p) {
    if (!p) return;
    if (S.starting && p.state === "downloading") {
      // the state event can beat start_download's invoke resolution
      S.dlId = p.id;
      S.starting = false;
    }
    if (p.id !== S.dlId) return;
    switch (p.state) {
      case "downloading":
        setState("downloading");
        if (p.note) showNote(p.note);
        if (S.lastProgress) applyProgress(S.lastProgress);
        break;
      case "paused":
        setState("paused");
        if (S.lastProgress) applyProgress(S.lastProgress);
        break;
      case "done":
        S.finalPath = typeof p.path === "string" ? p.path : null;
        els.doneSize.textContent = p.size != null
          ? formatBytes(p.size) + " downloaded"
          : "Download complete";
        els.donePath.textContent = S.finalPath ? middleEllipsis(S.finalPath, 56) : "";
        setState("done");
        break;
      case "error":
        showError(p.message || "download failed");
        break;
    }
  }

  function onProgress(p) {
    if (!p || p.id !== S.dlId) return;
    if (S.state !== "downloading" && S.state !== "paused" && !S.starting) return;
    S.lastProgress = p;
    if (!S.rafPending) {
      S.rafPending = true;
      requestAnimationFrame(flushProgress);
    }
  }

  // Coalesce >= 4 Hz event streams into at most one DOM pass per frame.
  function flushProgress() {
    S.rafPending = false;
    if (!S.lastProgress) return;
    if (S.state !== "downloading" && S.state !== "paused" && !S.starting) return;
    applyProgress(S.lastProgress);
  }

  function applyProgress(p) {
    var known = p.total != null && p.total > 0;

    els.barTrack.classList.toggle("indeterminate", !known);
    if (known) {
      var overall = pct(p.bytesDone, p.total);
      els.barFill.style.width = (overall == null ? 0 : overall).toFixed(2) + "%";
    } else if (els.barFill.style.width !== "") {
      // clear the inline width so .indeterminate's CSS strip can show
      els.barFill.style.width = "";
    }

    var speedText = formatSpeed(p.speedBps);
    els.speedReadout.textContent = speedText || "";

    if (S.state === "paused") {
      var p2 = pct(p.bytesDone, p.total);
      els.stBytes.textContent = p2 != null ? "Paused - " + p2.toFixed(1) + "%" : "Paused";
      els.stSpeed.textContent = "";
      els.stEta.textContent = "";
      return;
    }

    els.stBytes.textContent = known
      ? formatBytes(p.bytesDone) + " of " + formatBytes(p.total)
      : formatBytes(p.bytesDone);
    els.stSpeed.textContent = speedText || "";
    els.stEta.textContent = formatEtaLeft(p.etaSecs) || "";

    applyConns(p);
  }

  // ---------- per-connection grid (built once per download, mutated per tick) ----------

  function buildConnGrid(n) {
    els.connGrid.textContent = "";
    els.connGrid.classList.toggle("single", n === 1);
    S.connCells = [];
    S.gridFor = n;
    for (var i = 0; i < n; i++) {
      var root = document.createElement("div");
      root.className = "conn";
      root.dataset.phase = "idle";
      var idx = document.createElement("span");
      idx.className = "conn-i";
      idx.textContent = String(i + 1);
      var track = document.createElement("div");
      track.className = "conn-track";
      var fill = document.createElement("div");
      fill.className = "conn-fill";
      fill.style.width = "0%";
      var pctEl = document.createElement("span");
      pctEl.className = "conn-pct";
      pctEl.textContent = "0%";
      track.appendChild(fill);
      root.appendChild(idx);
      root.appendChild(track);
      root.appendChild(pctEl);
      els.connGrid.appendChild(root);
      S.connCells.push({ root: root, fill: fill, pct: pctEl });
    }
  }

  function applyConns(p) {
    if (!Array.isArray(p.conns) || p.conns.length === 0) return;
    // Structural change only when the backend's connection count changes
    // (e.g. range-request fallback to a single stream) or a new download starts.
    if (p.conns.length !== S.gridFor) buildConnGrid(p.conns.length);
    for (var i = 0; i < p.conns.length && i < S.connCells.length; i++) {
      var c = p.conns[i];
      var cell = S.connCells[i];
      var phase = c.phase || "idle";
      if (cell.root.dataset.phase !== phase) cell.root.dataset.phase = phase;
      var cp = pct(c.done, c.size);
      if (cp == null) {
        // unknown chunk size: indeterminate activity bar
        if (!cell.fill.classList.contains("indeterminate")) {
          cell.fill.style.width = "";
          cell.fill.classList.add("indeterminate");
        }
        if (cell.pct.textContent !== "") cell.pct.textContent = "";
      } else {
        if (cell.fill.classList.contains("indeterminate")) cell.fill.classList.remove("indeterminate");
        var w = cp.toFixed(2) + "%";
        if (cell.fill.style.width !== w) cell.fill.style.width = w;
        var label = phase === "done" ? "100%" : Math.floor(cp) + "%";
        if (cell.pct.textContent !== label) cell.pct.textContent = label;
      }
    }
  }

  // ---------- wiring ----------

  els.urlInput.addEventListener("paste", function () {
    setTimeout(maybeProbe, 0); // let the pasted text land first
  });

  els.urlInput.addEventListener("keydown", function (e) {
    if (e.key === "Enter") {
      e.preventDefault();
      maybeProbe();
    }
  });

  var metaTimer = 0;

  els.filenameInput.addEventListener("input", function () {
    if (S.state !== "ready") return;
    clearTimeout(metaTimer);
    metaTimer = setTimeout(function () {
      if (S.state === "ready" && S.settings.destDir) metaCheck();
    }, 350);
  });

  els.filenameInput.addEventListener("keydown", function (e) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (S.state === "ready") els.downloadBtn.click();
    }
  });

  els.chooseBtn.addEventListener("click", async function () {
    try {
      var dir = await invoke("pick_folder", {});
      if (dir == null) return; // picker cancelled
      S.settings.destDir = dir;
      renderDestRow();
      updateDownloadButton();
      els.folderPrompt.hidden = true;
      invoke("save_settings", { destDir: dir }).catch(function () { /* best-effort */ });
      metaCheck();
    } catch (err) {
      showFolderPrompt(errText(err), true);
    }
  });

  els.connMinus.addEventListener("click", function () { stepConnections(-1); });
  els.connPlus.addEventListener("click", function () { stepConnections(1); });

  els.downloadBtn.addEventListener("click", function () {
    if (S.state !== "ready" || !S.probe) return;
    if (!S.settings.destDir) {
      showFolderPrompt("Choose a folder to continue", false);
      attnSaveRow();
      return;
    }
    startDownload();
  });

  els.pauseBtn.addEventListener("click", async function () {
    if (S.state !== "downloading" || !S.dlId) return;
    els.pauseBtn.disabled = true;
    try {
      await invoke("pause_download", { id: S.dlId });
    } catch (err) {
      els.pauseBtn.disabled = false;
      showError(errText(err));
    }
  });

  els.resumeBtn.addEventListener("click", async function () {
    if (S.state !== "paused" || !S.dlId) return;
    els.resumeBtn.disabled = true;
    try {
      await invoke("resume_download", { id: S.dlId });
    } catch (err) {
      els.resumeBtn.disabled = false;
      showError(errText(err));
    }
  });

  els.cancelBtn.addEventListener("click", async function () {
    if ((S.state !== "downloading" && S.state !== "paused") || !S.dlId) return;
    els.cancelBtn.disabled = true;
    try {
      await invoke("cancel_download", { id: S.dlId });
    } catch (err) {
      els.cancelBtn.disabled = false;
      showError(errText(err));
      return;
    }
    // contract: cancel emits no state event - the UI resets itself to idle
    S.dlId = null;
    S.starting = false;
    S.lastProgress = null;
    els.urlMsgText.textContent = "";
    els.urlMsg.classList.remove("is-error");
    setState("idle");
  });

  els.retryBtn.addEventListener("click", function () {
    if (S.state !== "error" || !S.probe) return;
    // "Try again": back to ready with the same parameters
    S.dlId = null;
    S.starting = false;
    S.lastProgress = null;
    setState("ready");
    if (S.settings.destDir) metaCheck();
  });

  els.revealBtn.addEventListener("click", async function () {
    if (S.state !== "done" || !S.finalPath) return;
    try {
      await invoke("reveal_path", { path: S.finalPath });
    } catch (err) {
      showNote(errText(err));
    }
  });

  // ---------- boot ----------

  async function init() {
    try {
      await window.__TAURI__.event.listen("hydra://state", function (e) { onState(e && e.payload); });
      await window.__TAURI__.event.listen("hydra://progress", function (e) { onProgress(e && e.payload); });
    } catch (err) { /* no event channel: nothing can drive the transfer states */ }

    try {
      var s = await invoke("get_settings", {});
      if (s) {
        if (typeof s.destDir === "string") S.settings.destDir = s.destDir;
        if (typeof s.connections === "number") S.settings.connections = clampConnections(s.connections);
      }
    } catch (err) { /* defaults stand */ }

    renderDestRow();
    refreshStepper();
    updateDownloadButton();
    setState("idle");
  }

  init();
})();
