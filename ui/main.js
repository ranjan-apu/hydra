/* main.js — Hydra frontend.
 *
 * Talks ONLY to window.__TAURI__ (Tauri v2, withGlobalTauri):
 *   await window.__TAURI__.core.invoke(name, args)   — camelCase arg keys per CONTRACT.md
 *   await window.__TAURI__.event.listen(name, cb)    — payload at cb.payload
 *
 * State machine: idle -> probing -> ready -> downloading -> paused -> done | error
 * (error -> ready via "Try again"; done -> idle via "Start another download").
 * Speed and ETA always come from the backend (hydra://progress); this file only
 * formats IEC bytes, MiB/s and "4m 12s".
 */

"use strict";

(function () {
  var $ = function (id) { return document.getElementById(id); };

  var els = {
    brandMark: $("brand-mark"),
    engineBadge: $("engine-badge"),
    statusPill: $("status-pill"),
    statusText: $("status-text"),

    sourceCard: $("source-card"),
    urlWrap: $("url-wrap"),
    urlInput: $("url-input"),
    probeBtn: $("probe-btn"),
    urlMsg: $("url-msg"),
    urlMsgText: $("url-msg-text"),
    errorBanner: $("error-banner"),
    errorMsg: $("error-msg"),

    readyCard: $("ready-card"),
    filenameInput: $("filename-input"),
    typeChip: $("type-chip"),
    metaSize: $("meta-size"),
    metaRanges: $("meta-ranges"),
    metaProbe: $("meta-probe"),
    metaServer: $("meta-server"),
    resumeNote: $("resume-note"),
    resumeText: $("resume-text"),

    destCard: $("dest-card"),
    chooseBtn: $("choose-btn"),
    destText: $("dest-text"),
    connRange: $("conn-range"),
    connValue: $("conn-value"),
    connNote: $("conn-note"),

    progressCard: $("progress-card"),
    dlName: $("dl-name"),
    progressPct: $("progress-pct"),
    barTrack: $("bar-track"),
    barFill: $("bar-fill"),
    statSpeed: $("stat-speed"),
    statBytes: $("stat-bytes"),
    statEta: $("stat-eta"),
    connSummaryRow: $("conn-summary-row"),
    connSummary: $("conn-summary"),
    connGrid: $("conn-grid"),
    stateNote: $("state-note"),
    transferError: $("transfer-error"),
    transferErrorMsg: $("transfer-error-msg"),
    donePanel: $("done-panel"),
    doneTitle: $("done-title"),
    donePath: $("done-path"),
    revealBtn: $("reveal-btn"),

    footerNote: $("footer-note"),
    actionHint: $("action-hint"),
    cancelBtn: $("cancel-btn"),
    pauseBtn: $("pause-btn"),
    primaryBtn: $("primary-btn")
  };

  var MIN_CONNECTIONS = 1;
  var MAX_CONNECTIONS = 16;

  var S = {
    state: "idle",
    settings: { destDir: null, connections: 8 },
    probe: null,
    dlId: null,
    starting: false,
    finalPath: null,
    lastProgress: null,
    rafPending: false,
    cells: [],
    gridFor: -1,
    metaSeq: 0,
    controlPending: false,
    ariaPct: -1,
    stepperLocked: false,
    note: "",
    liveConns: 0
  };

  var STATUS = {
    idle: { label: "Idle", tone: "idle" },
    probing: { label: "Probing", tone: "busy" },
    ready: { label: "Ready", tone: "ready" },
    downloading: { label: "Downloading", tone: "live" },
    paused: { label: "Paused", tone: "warn" },
    done: { label: "Complete", tone: "ok" },
    error: { label: "Failed", tone: "error" }
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
    if (bps == null || !isFinite(bps)) return "\u2014";
  if (bps <= 0) return "0.0 MiB/s";
    return (bps / 1048576).toFixed(1) + " MiB/s";
  }

  function formatEta(secs) {
    if (secs == null || !isFinite(secs) || secs < 0) return "\u2014";
    secs = Math.round(secs);
    if (secs < 60) return secs + "s";
    if (secs < 3600) {
      return Math.floor(secs / 60) + "m " + String(secs % 60).padStart(2, "0") + "s";
    }
    return Math.floor(secs / 3600) + "h " + String(Math.floor((secs % 3600) / 60)).padStart(2, "0") + "m";
  }

  function middleEllipsis(text, max) {
    text = String(text || "");
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

  // ---------- state ----------

  function setState(next) {
    S.state = next;
    document.body.dataset.state = next;
    var s = STATUS[next] || STATUS.idle;
    els.statusText.textContent = s.label;
    els.statusPill.dataset.tone = s.tone;
    render();
  }

  function currentFilename() {
    var v = els.filenameInput.value.trim();
    if (v) return v;
    return S.probe ? S.probe.filename : "";
  }

  // ---------- render ----------

  function render() {
    var st = S.state;
    var active = st === "downloading" || st === "paused";
    var failed = st === "error";
    var finished = st === "done";
    var live = active || failed;

    // A live run owns the window: the setup cards step aside so the transfer,
    // its numbers and its controls are the only thing in view.
    var runOwnsWindow = live || finished;
    els.sourceCard.hidden = runOwnsWindow;
    els.readyCard.hidden = runOwnsWindow;
    els.destCard.hidden = runOwnsWindow;
    els.readyCard.classList.toggle("is-empty", !S.probe);
    els.progressCard.hidden = !runOwnsWindow;

    els.urlInput.disabled = active;
    els.probeBtn.disabled = active || st === "probing" || !els.urlInput.value.trim();
    els.filenameInput.disabled = active;
    els.connRange.disabled = active || S.stepperLocked;
    els.chooseBtn.disabled = active;

    // transfer surface
    els.barTrack.hidden = !(live || finished);
    els.connSummaryRow.hidden = !(live || finished);
    els.connGrid.hidden = S.liveConns <= 1;
    els.stateNote.hidden = !S.note;
    if (S.note) els.stateNote.textContent = S.note;
    els.transferError.hidden = !failed;
    els.donePanel.hidden = !finished;

    els.pauseBtn.hidden = !active;
    els.cancelBtn.hidden = !active;
    els.pauseBtn.textContent = st === "paused" ? "Resume" : "Pause";
    els.pauseBtn.className = st === "paused" ? "btn btn-primary" : "btn btn-secondary";

    els.urlMsg.hidden = st !== "probing" && !els.urlMsgText.textContent.trim();

    // primary action
    els.primaryBtn.hidden = active;
    if (st === "done") {
      els.primaryBtn.textContent = "Start another download";
      els.primaryBtn.disabled = false;
    } else if (failed) {
      els.primaryBtn.textContent = "Try again";
      els.primaryBtn.disabled = !S.probe;
    } else {
      els.primaryBtn.textContent = "Start download";
      els.primaryBtn.disabled = !S.probe || !S.settings.destDir;
    }

    renderActionHint();
    renderDest();
  }

  function renderActionHint() {
    var st = S.state;
    var text = "";
    var tone = "";
    if (st === "downloading") {
      var n = S.liveConns || S.settings.connections;
      text = "Streaming " + n + (n === 1 ? " connection" : " connections");
    } else if (st === "paused") {
      text = "Paused, offsets saved";
    } else if (st === "error") {
      text = "Download failed";
      tone = "error";
    } else if (st === "done") {
      text = S.settings.destDir ? "Saved to " + middleEllipsis(S.settings.destDir, 34) : "Saved";
    } else if (st === "ready" && !S.settings.destDir) {
      text = "Choose a destination folder to continue";
      tone = "warn";
    } else if (st === "ready" && S.probe && !S.stepperLocked) {
      var c = S.settings.connections;
      text = c + (c === 1 ? " connection" : " connections");
    }
    els.actionHint.textContent = text;
    els.actionHint.hidden = !text;
    if (tone) els.actionHint.dataset.tone = tone;
    else els.actionHint.removeAttribute("data-tone");
  }

  function renderDest() {
    if (S.settings.destDir) {
      els.destText.textContent = S.settings.destDir;
      els.destText.classList.remove("is-empty");
    } else {
      els.destText.textContent = "No folder chosen";
      els.destText.classList.add("is-empty");
    }
  }

  function renderStepper() {
    var n = S.settings.connections;
    els.connRange.value = String(n);
    els.connValue.textContent = String(n);
    els.connRange.style.setProperty("--fill", ((n - MIN_CONNECTIONS) / (MAX_CONNECTIONS - MIN_CONNECTIONS) * 100) + "%");
    els.connRange.setAttribute("aria-valuetext", n + (n === 1 ? " connection" : " connections"));
  }

  function renderUrlMsg() {
    var st = S.state;
    els.urlMsg.classList.toggle("is-busy", st === "probing");
    els.urlWrap.classList.toggle("is-invalid", !!(S.urlError && st !== "probing"));
  }

  // ---------- errors ----------

  function showUrlError(message) {
    S.urlError = true;
    els.errorMsg.textContent = message;
    els.errorBanner.hidden = false;
    renderUrlMsg();
    els.urlWrap.classList.add("is-invalid");
  }

  function clearUrlError() {
    S.urlError = false;
    els.errorBanner.hidden = true;
    els.errorMsg.textContent = "";
    els.urlWrap.classList.remove("is-invalid");
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
    clearUrlError();
    setState("probing");
    els.urlMsgText.textContent = "Checking the origin\u2026";
    try {
      var info = await invoke("probe_url", { url: url });
      if (!info || typeof info !== "object") throw new Error("empty probe response");
      S.probe = info;
      S.metaSeq++;
      populateDetails(info);
      els.urlMsgText.textContent = "";
      setState("ready");
      if (S.settings.destDir) metaCheck();
    } catch (err) {
      S.probe = null;
      els.urlMsgText.textContent = "";
      setState("idle");
      showUrlError(errText(err));
    }
  }

  function populateDetails(info) {
    els.filenameInput.value = info.filename || "";
    els.filenameInput.title = info.filename || "";

    els.metaSize.textContent = info.size == null ? "Unknown" : formatBytes(info.size);
    els.metaSize.style.color = info.size == null ? "var(--muted)" : "";

    var multi = !!info.acceptsRanges && info.size != null;
    els.metaRanges.textContent = info.acceptsRanges ? "Supported" : "Not supported";
    els.metaRanges.className = "badge " + (info.acceptsRanges ? "badge-yes" : "badge-no");

    var method = info.probeMethod === "range" ? "Range GET" : "HEAD";
    if (info.acceptsRanges && info.probeMethod === "head") method = "HEAD";
    els.metaProbe.textContent = method;

    els.metaServer.textContent = info.server || "Not advertised";
    els.metaServer.title = info.server || "";

    if (info.contentType) {
      els.typeChip.textContent = info.contentType;
      els.typeChip.title = info.contentType;
      els.typeChip.hidden = false;
    } else {
      els.typeChip.hidden = true;
    }

    els.resumeNote.hidden = true;

    S.stepperLocked = !multi;
    els.connNote.hidden = multi;
    els.connNote.textContent = multi
      ? ""
      : (info.size == null ? "Unknown file size, so one connection" : "This server ignores range requests, so one connection");
  }

  // ---------- resume detection ----------

  async function metaCheck() {
    if (!S.probe || !S.settings.destDir || S.state !== "ready") {
      els.resumeNote.hidden = true;
      return;
    }
    var seq = ++S.metaSeq;
    try {
      var res = await invoke("meta_check", {
        url: S.probe.url,
        destDir: S.settings.destDir,
        filename: currentFilename(),
        size: S.probe.size
      });
      if (seq !== S.metaSeq || S.state !== "ready") return;
      var p = pct(res && res.bytesDone, S.probe.size);
      var show = !!(res && res.compatible && res.bytesDone > 0 && p != null && p > 0);
      els.resumeNote.hidden = !show;
      if (show) {
        els.resumeText.textContent = "Partial file found: " + formatBytes(res.bytesDone) +
          " of " + formatBytes(S.probe.size) + " done. Start will resume it.";
      }
    } catch (err) {
      if (seq === S.metaSeq) els.resumeNote.hidden = true;
    }
  }

  // ---------- transfer surface ----------

  function resetTransferSurface() {
    els.barFill.style.width = "0%";
    els.barTrack.classList.remove("is-indeterminate");
    els.barTrack.setAttribute("aria-valuenow", "0");
    els.barTrack.setAttribute("aria-valuetext", "");
    els.progressPct.textContent = "0%";
    els.statSpeed.textContent = "0.0 MiB/s";
    els.statBytes.textContent = S.probe && S.probe.size != null
      ? "0 B / " + formatBytes(S.probe.size)
      : "0 B";
    els.statEta.textContent = "calculating\u2026";
    els.connSummary.textContent = "0/" + S.settings.connections + " complete";
    S.note = "";
    els.stateNote.hidden = true;
    els.stateNote.classList.remove("is-error");
    els.stateNote.textContent = "";
    els.transferError.hidden = true;
    els.transferErrorMsg.textContent = "";
    els.donePath.textContent = "";
    els.connGrid.textContent = "";
    els.connGrid.removeAttribute("data-count");
    S.cells = [];
    S.gridFor = -1;
    S.ariaPct = -1;
    S.liveConns = S.settings.connections; // optimistic until the first event
    els.connGrid.hidden = S.liveConns <= 1;
  }

  async function startDownload() {
    if (!S.probe || S.starting || !S.settings.destDir) return;
    var args = {
      url: S.probe.url,
      destDir: S.settings.destDir,
      filename: currentFilename(),
      connections: S.settings.connections,
      size: S.probe.size,
      acceptsRanges: S.probe.acceptsRanges
    };
    S.starting = true;
    S.lastProgress = null;
    S.finalPath = null;
    resetTransferSurface();
    els.dlName.textContent = args.filename;
    els.dlName.title = args.filename;
    clearUrlError();
    try {
      var res = await invoke("start_download", args);
      if (S.starting && res && res.id) S.dlId = res.id;
      setState("downloading");
    } catch (err) {
      S.starting = false;
      showTransferError(errText(err));
    }
  }

  function showTransferError(message) {
    els.transferErrorMsg.textContent = message;
    setState("error");
    // keep the last measured frame on screen: where it failed is information.
    // Applied after the state flips so the dead run reports no live speed/ETA.
    if (S.lastProgress) {
      try { applyProgress(S.lastProgress); } catch (e) { /* never mask the real error */ }
    }
  }

  // ---------- events ----------

  function onState(p) {
    if (!p) return;
    if (S.starting && p.state === "downloading") {
      S.dlId = p.id;
      S.starting = false;
    }
    if (p.id != null && S.dlId != null && p.id !== S.dlId) return;

    switch (p.state) {
      case "downloading":
        clearUrlError();
        ensureTitle();
        setState("downloading");
        if (p.note) showNote(p.note);
        if (S.lastProgress) applyProgress(S.lastProgress);
        break;
      case "paused":
        setState("paused");
        showNote("Paused, offsets saved");
        if (S.lastProgress) applyProgress(S.lastProgress);
        break;
      case "done":
        ensureTitle();
        if (S.lastProgress) {
          try { applyProgress(S.lastProgress); } catch (e) { /* the panel below is the truth */ }
        }
        els.barFill.style.width = "100%";
        els.barTrack.classList.remove("is-indeterminate");
        els.progressPct.textContent = "100%";
        els.barTrack.setAttribute("aria-valuenow", "100");
        els.barTrack.setAttribute("aria-valuetext", "100 percent, download complete");
        S.finalPath = typeof p.path === "string" ? p.path : null;
        els.donePath.textContent = S.finalPath ? middleEllipsis(S.finalPath, 58) : "";
        els.donePath.title = S.finalPath || "";
        els.doneTitle.textContent = p.size != null
          ? "Download complete \u00b7 " + formatBytes(p.size)
          : "Download complete";
        setState("done");
        break;
      case "error":
        showTransferError(p.message || "download failed");
        break;
    }
  }

  function ensureTitle() {
    var t = els.dlName.textContent.trim();
    if (!t) {
      els.dlName.textContent = currentFilename() || "download";
      els.dlName.title = els.dlName.textContent;
    }
  }

  function showNote(text) {
    S.note = text || "";
    els.stateNote.textContent = S.note;
    els.stateNote.hidden = !S.note;
  }

  function onProgress(p) {
    if (!p) return;
    if (p.id != null && S.dlId != null && p.id !== S.dlId) return;
    if (S.state !== "downloading" && S.state !== "paused" && S.state !== "error" && !S.starting) return;
    S.lastProgress = p;
    if (!S.rafPending) {
      S.rafPending = true;
      requestAnimationFrame(flushProgress);
    }
  }

  // Coalesce the >= 4 Hz stream into at most one DOM pass per frame.
  function flushProgress() {
    S.rafPending = false;
    if (!S.lastProgress) return;
    if (S.state !== "downloading" && S.state !== "paused" && S.state !== "error" && !S.starting) return;
    applyProgress(S.lastProgress);
  }

  function applyProgress(p) {
    var known = p.total != null && p.total > 0;
    var overall = known ? pct(p.bytesDone, p.total) : null;

    els.barTrack.classList.toggle("is-indeterminate", !known);
    if (known) {
      els.barFill.style.width = (overall == null ? 0 : overall).toFixed(2) + "%";
    } else {
      els.barFill.style.width = "";
    }

    // the accessible value only changes when the whole percent does
    var whole = known && overall != null ? Math.floor(overall) : 0;
    if (whole !== S.ariaPct) {
      S.ariaPct = whole;
      els.barTrack.setAttribute("aria-valuenow", String(whole));
      els.barTrack.setAttribute("aria-valuetext", known
        ? formatBytes(p.bytesDone) + " of " + formatBytes(p.total) + ", " + whole + " percent"
        : formatBytes(p.bytesDone) + " downloaded");
    }

    if (known && overall != null) {
      els.progressPct.textContent = (overall >= 100 ? 100 : overall).toFixed(1) + "%";
    } else {
      els.progressPct.textContent = formatBytes(p.bytesDone);
    }

    var stalled = S.state === "paused" || S.state === "error";
    els.statSpeed.textContent = stalled ? "\u2014" : formatSpeed(p.speedBps);
    els.statBytes.textContent = known
      ? formatBytes(p.bytesDone) + " / " + formatBytes(p.total)
      : formatBytes(p.bytesDone);
    els.statEta.textContent = stalled ? "\u2014" : formatEta(p.etaSecs);

    applyConns(p);
  }

  // ---------- the comb (built once per download, mutated per tick) ----------

  function buildConnGrid(n) {
    els.connGrid.textContent = "";
    S.cells = [];
    S.gridFor = n;
    els.connGrid.dataset.count = String(n);
    for (var i = 0; i < n; i++) {
      var root = document.createElement("div");
      root.className = "conn-cell";
      root.dataset.phase = "idle";

      var num = document.createElement("span");
      num.className = "conn-num";
      num.textContent = String(i + 1);

      var track = document.createElement("div");
      track.className = "conn-track";
      var fill = document.createElement("div");
      fill.className = "conn-fill";
      fill.style.height = "0%";
      track.appendChild(fill);

      var pctEl = document.createElement("span");
      pctEl.className = "conn-pct";
      pctEl.textContent = "0%";

      root.appendChild(num);
      root.appendChild(track);
      root.appendChild(pctEl);
      els.connGrid.appendChild(root);
      S.cells.push({ root: root, fill: fill, pct: pctEl, label: "0%" });
    }
  }

  function applyConns(p) {
    if (!Array.isArray(p.conns) || p.conns.length === 0) return;
    if (p.conns.length !== S.gridFor) buildConnGrid(p.conns.length);

    S.liveConns = p.conns.length;
    els.connGrid.hidden = S.liveConns <= 1;
    var doneCount = 0;
    for (var i = 0; i < p.conns.length && i < S.cells.length; i++) {
      var c = p.conns[i];
      var cell = S.cells[i];
      var phase = c.phase || "idle";
      if (phase === "done") doneCount++;
      if (cell.root.dataset.phase !== phase) cell.root.dataset.phase = phase;

      var cp = pct(c.done, c.size);
      if (cp == null) {
        if (cell.fill.style.height !== "") cell.fill.style.height = "";
        if (cell.label !== "") { cell.pct.textContent = ""; cell.label = ""; }
      } else {
        var h = cp.toFixed(1) + "%";
        if (cell.fill.style.height !== h) cell.fill.style.height = h;
        var label = phase === "done" ? "100%" : Math.floor(cp) + "%";
        if (cell.label !== label) { cell.pct.textContent = label; cell.label = label; }
      }
    }
    els.connSummary.textContent = S.liveConns === 1
      ? (doneCount === 1 ? "single stream, complete" : "single stream")
      : doneCount + "/" + p.conns.length + " complete";
  }

  // ---------- wiring ----------

  els.urlInput.addEventListener("paste", function () {
    setTimeout(maybeProbe, 0);
  });
  els.urlInput.addEventListener("keydown", function (e) {
    if (e.key === "Enter") { e.preventDefault(); maybeProbe(); }
  });
  els.urlInput.addEventListener("input", function () {
    clearUrlError();
    if (S.probe) {
      S.probe = null;
      els.resumeNote.hidden = true;
      if (S.state !== "probing") setState("idle");
    }
    els.probeBtn.disabled = !els.urlInput.value.trim();
  });
  els.probeBtn.addEventListener("click", function () { maybeProbe(); });

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
      if (S.state === "ready" && !els.primaryBtn.disabled) els.primaryBtn.click();
    }
  });

  els.chooseBtn.addEventListener("click", async function () {
    try {
      var dir = await invoke("pick_folder", {});
      if (dir == null) return;
      S.settings.destDir = dir;
      render();
      invoke("save_settings", { destDir: dir }).catch(function () {});
      metaCheck();
    } catch (err) {
      showUrlError(errText(err));
    }
  });

  els.connRange.addEventListener("input", function () {
    var n = clampConnections(els.connRange.value);
    S.settings.connections = n;
    renderStepper();
    renderActionHint();
    invoke("save_settings", { connections: n }).catch(function () {});
  });

  els.primaryBtn.addEventListener("click", function () {
    if (S.state === "done") {
      S.probe = null;
      S.dlId = null;
      S.lastProgress = null;
      S.finalPath = null;
      els.resumeNote.hidden = true;
      resetTransferSurface();
      setState("idle");
      els.urlInput.focus();
      els.urlInput.select();
      return;
    }
    if (S.state === "error") {
      S.dlId = null;
      S.starting = false;
      S.lastProgress = null;
      els.transferError.hidden = true;
      setState("ready");
      if (S.settings.destDir) metaCheck();
      return;
    }
    if (S.state === "ready" && S.probe) {
      if (!S.settings.destDir) {
        els.actionHint.textContent = "Choose a destination folder to continue";
        els.actionHint.hidden = false;
        els.actionHint.dataset.tone = "warn";
        els.chooseBtn.focus();
        return;
      }
      startDownload();
    }
  });

  async function togglePause() {
    if (S.controlPending) return;
    if (S.state !== "downloading" && S.state !== "paused") return;
    var resuming = S.state === "paused";
    S.controlPending = true;
    // Optimistic: the 4 Hz progress stream must not race this await.
    setState(resuming ? "downloading" : "paused");
    try {
      if (resuming) await invoke("resume_download", { id: S.dlId });
      else await invoke("pause_download", { id: S.dlId });
    } catch (err) {
      setState(resuming ? "paused" : "downloading");
      showTransferError(errText(err));
    } finally {
      S.controlPending = false;
    }
  }

  els.pauseBtn.addEventListener("click", togglePause);

  els.cancelBtn.addEventListener("click", async function () {
    if ((S.state !== "downloading" && S.state !== "paused") || !S.dlId) return;
    els.cancelBtn.disabled = true;
    try {
      await invoke("cancel_download", { id: S.dlId });
    } catch (err) {
      els.cancelBtn.disabled = false;
      showTransferError(errText(err));
      return;
    }
    els.cancelBtn.disabled = false;
    // cancel leaves the probe valid, so return straight to ready
    S.dlId = null;
    S.starting = false;
    S.lastProgress = null;
    resetTransferSurface();
    setState("ready");
    metaCheck();
  });

  els.revealBtn.addEventListener("click", async function () {
    if (!S.finalPath) return;
    try {
      await invoke("reveal_path", { path: S.finalPath });
    } catch (err) {
      els.donePath.textContent = errText(err);
      els.donePath.classList.add("is-error");
    }
  });

  // ---------- boot ----------

  async function init() {
    var backend = false;
    try {
      await window.__TAURI__.event.listen("hydra://state", function (e) { onState(e && e.payload); });
      await window.__TAURI__.event.listen("hydra://progress", function (e) { onProgress(e && e.payload); });
      backend = true;
    } catch (err) { /* no event channel: nothing can drive the transfer states */ }

    try {
      var s = await invoke("get_settings", {});
      if (s && typeof s === "object") {
        if (typeof s.destDir === "string") S.settings.destDir = s.destDir;
        if (typeof s.connections === "number") S.settings.connections = clampConnections(s.connections);
      }
      backend = true;
    } catch (err) { /* defaults stand */ }

    renderStepper();
    els.probeBtn.disabled = true;
    setState("idle");

    if (backend) {
      els.engineBadge.hidden = false;
      setTimeout(function () { els.engineBadge.hidden = true; }, 2400);
    }
  }

  init();
})();
