/* main.js — Hydra frontend.

 * One window, many downloads: a sidebar lists every run (live and finished) and
 * the pane shows either the compose form or the selected run's detail.
 *
 * Talks ONLY to window.__TAURI__ (Tauri v2, withGlobalTauri):
 *   await window.__TAURI__.core.invoke(name, args)   — camelCase keys per CONTRACT.md
 *   await window.__TAURI__.event.listen(name, cb)    — payload at cb.payload
 *
 * Two independent state machines:
 *   - the compose form in the pane: idle -> probing -> ready
 *   - each download item in the sidebar: downloading -> paused -> done | error
 * Speed and ETA always come from the backend; this file only formats.
 */

"use strict";

(function () {
  var $ = function (id) { return document.getElementById(id); };

  var els = {
    brandMark: $("brand-mark"),
    engineBadge: $("engine-badge"),
    statusPill: $("status-pill"),
    statusText: $("status-text"),

    newBtn: $("new-btn"),
    sidebarCount: $("sidebar-count"),
    dlList: $("dl-list"),
    listEmpty: $("list-empty"),

    composeView: $("compose-view"),
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
    footerNote: $("footer-note"),

    detailView: $("detail-view"),
    dlName: $("dl-name"),
    dlSub: $("dl-sub"),
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

    actionHint: $("action-hint"),
    cancelBtn: $("cancel-btn"),
    pauseBtn: $("pause-btn"),
    removeBtn: $("remove-btn"),
    revealBtn: $("reveal-btn"),
    primaryBtn: $("primary-btn")
  };

  var MIN_CONNECTIONS = 1;
  var MAX_CONNECTIONS = 16;
  var MAX_CONCURRENT = 8;

  var S = {
    view: "compose",              // "compose" | download id
    order: [],                    // ids, newest first
    items: {},                    // id -> item
    rowEls: {},                   // id -> { li, btn, name, pct, fill, state }

    settings: { destDir: null, connections: 8 },
    probe: null,
    probeSeq: 0,
    metaSeq: 0,
    composeState: "idle",         // idle | probing | ready
    composeError: "",
    starting: false,

    rafPending: false,
    dirty: {},
    gridKey: "",
    cells: [],
    ariaPct: -1,
    liveConns: 0,
    controlBusy: {},              // id -> true while a pause/cancel is in flight
    pending: null,                // authoritative args of a start that has not acked yet
    adoptedId: null,              // id adopted from an event that beat that ack
    deadIds: {},
    unlisten: []
  };

  function invoke(name, args) {
    return window.__TAURI__.core.invoke(name, args);
  }

  function errText(err) {
    if (typeof err === "string") return err;
    if (err && typeof err.message === "string") return err.message;
    return String(err);
  }

  // ---------- formatting ----------

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
    if (secs < 3600) return Math.floor(secs / 60) + "m " + String(secs % 60).padStart(2, "0") + "s";
    return Math.floor(secs / 3600) + "h " + String(Math.floor((secs % 3600) / 60)).padStart(2, "0") + "m";
  }

  function middleEllipsis(text, max) {
    text = String(text || "");
    if (text.length <= max) return text;
    var head = Math.ceil((max - 1) / 2);
    var tail = Math.floor((max - 1) / 2);
    return text.slice(0, head) + "\u2026" + text.slice(text.length - tail);
  }

  function pctOf(done, total) {
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

  var STATE_LABEL = {
    downloading: "downloading",
    paused: "paused",
    done: "complete",
    error: "failed",
    ready: "starting"
  };

  // ---------- items ----------

  function activeCount() {
    var n = 0;
    for (var i = 0; i < S.order.length; i++) {
      var st = S.items[S.order[i]].state;
      if (st === "downloading" || st === "paused") n++;
    }
    return n;
  }

  function selected() {
    return S.view === "compose" ? null : S.items[S.view] || null;
  }

  function makeItem(id, args) {
    return {
      id: id,
      url: args.url,
      filename: args.filename,
      destDir: args.destDir,
      connections: args.connections,
      size: args.size,
      acceptsRanges: args.acceptsRanges,
      state: "downloading",
      bytesDone: 0,
      total: args.size,
      speedBps: 0,
      etaSecs: null,
      conns: [],
      note: "",
      error: "",
      path: null,
      doneSize: null
    };
  }

  // Idempotent on purpose: an event can beat the start ack, in which case the row
  // already exists when the ack lands and must be reconciled rather than appended
  // (a second entry would strand a row no counter ever updates again).
  function addItem(item) {
    var existing = S.items[item.id];
    if (existing) {
      if (item.url) existing.url = item.url;
      if (item.filename) existing.filename = item.filename;
      if (item.destDir) existing.destDir = item.destDir;
      if (item.connections) existing.connections = item.connections;
      if (item.size != null) {
        existing.size = item.size;
        if (existing.total == null) existing.total = item.size;
      }
      if (item.acceptsRanges != null) existing.acceptsRanges = item.acceptsRanges;
      updateRow(item.id);
      return existing;
    }
    S.items[item.id] = item;
    S.order.unshift(item.id);
    rebuildList();
    return item;
  }

  function dropItem(id) {
    delete S.items[id];
    var at = S.order.indexOf(id);
    if (at >= 0) S.order.splice(at, 1);
    delete S.rowEls[id];
    delete S.dirty[id];
    delete S.deadIds[id];
    rebuildList();
    if (S.view === id) setView("compose", false);
    renderChrome();
  }

  // ---------- sidebar ----------

  function rebuildList() {
    els.dlList.textContent = "";
    S.rowEls = {};
    for (var i = 0; i < S.order.length; i++) {
      S.rowEls[S.order[i]] = buildRow(S.items[S.order[i]]);
    }
    S.order.forEach(function (id) { els.dlList.appendChild(S.rowEls[id].li); });
    els.listEmpty.hidden = S.order.length > 0;
  }

  function buildRow(item) {
    var li = document.createElement("li");
    li.className = "dl-item";

    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "dl-row";
    btn.setAttribute("aria-label", item.filename + ", " + (STATE_LABEL[item.state] || item.state));

    var top = document.createElement("span");
    top.className = "dl-row-top";
    var name = document.createElement("span");
    name.className = "dl-row-name";
    name.textContent = item.filename;
    name.title = item.filename;
    var pct = document.createElement("span");
    pct.className = "dl-row-pct";
    top.appendChild(name);
    top.appendChild(pct);

    var bar = document.createElement("span");
    bar.className = "dl-row-bar";
    var fill = document.createElement("span");
    fill.className = "dl-row-fill";
    bar.appendChild(fill);

    var foot = document.createElement("span");
    foot.className = "dl-row-foot";
    var state = document.createElement("span");
    state.className = "dl-row-state";
    state.textContent = STATE_LABEL[item.state] || item.state;
    foot.appendChild(state);

    btn.appendChild(top);
    btn.appendChild(bar);
    btn.appendChild(foot);
    btn.addEventListener("click", function () { setView(item.id); });

    var x = document.createElement("button");
    x.type = "button";
    x.className = "dl-row-x";
    x.addEventListener("click", function (e) {
      e.stopPropagation();
      closeItem(item.id);
    });

    li.appendChild(btn);
    li.appendChild(x);
    var row = { li: li, btn: btn, name: name, pct: pct, fill: fill, state: state, x: x };
    paintRow(row, item);
    return row;
  }

  // Mutate, never rebuild: progress ticks touch these rows at 4 Hz.
  function paintRow(row, item) {
    var p = pctOf(item.bytesDone, item.total);
    row.btn.dataset.state = item.state;
    row.btn.setAttribute("aria-current", S.view === item.id ? "true" : "false");
    row.li.dataset.state = item.state;
    if (row.state.textContent !== (STATE_LABEL[item.state] || item.state)) {
      row.state.textContent = STATE_LABEL[item.state] || item.state;
    }
    if (item.state === "done") {
      row.pct.textContent = "100%";
      row.fill.style.width = "100%";
    } else if (p == null) {
      row.pct.textContent = item.total === null ? "" : "";
      row.fill.style.width = "0%";
    } else {
      row.pct.textContent = Math.floor(p) + "%";
      row.fill.style.width = p.toFixed(1) + "%";
    }
    if (item.state === "error") row.pct.textContent = p == null ? "" : Math.floor(p) + "%";
    if (row.x.textContent !== "\u00d7") row.x.textContent = "\u00d7";
    if (row.label !== item.state) {
      row.label = item.state;
      row.x.setAttribute(
        "aria-label",
        item.state === "downloading" || item.state === "paused"
          ? "Cancel " + item.filename + " and delete its partial data"
          : "Remove " + item.filename + " from the list"
      );
    }
    var btnLabel = item.filename + ", " + (STATE_LABEL[item.state] || item.state);
    if (row.btnLabel !== btnLabel) {
      row.btnLabel = btnLabel;
      row.btn.setAttribute("aria-label", btnLabel);
    }
  }

  function updateRow(id) {
    var item = S.items[id];
    var row = S.rowEls[id];
    if (item && row) paintRow(row, item);
  }

  // ---------- view ----------

  function setView(view, announce) {
    S.view = view;
    S.ariaPct = -1;
    document.body.dataset.view = view === "compose" ? "compose" : "detail";
    els.composeView.hidden = view !== "compose";
    els.detailView.hidden = view === "compose";
    els.newBtn.setAttribute("aria-current", view === "compose" ? "true" : "false");
    for (var id in S.rowEls) {
      S.rowEls[id].btn.setAttribute("aria-current", view === id ? "true" : "false");
    }
    if (view === "compose") {
      S.gridKey = "";
      renderCompose();
    } else {
      renderDetail(true);
      if (announce) {
        var row = S.rowEls[view];
        if (row) row.btn.focus();
      }
    }
    renderChrome();
  }

  function composeStateOf() {
    return S.composeState;
  }

  function bodyState() {
    return S.view === "compose" ? composeStateOf() : (selected() ? selected().state : "idle");
  }

  function renderChrome() {
    var busy = 0;
    var paused = 0;
    for (var i = 0; i < S.order.length; i++) {
      var st = S.items[S.order[i]].state;
      if (st === "downloading") busy++;
      else if (st === "paused") paused++;
    }
    document.body.dataset.state = bodyState();
    document.body.dataset.busy = busy > 0 ? "true" : "false";

    var label;
    var tone;
    if (busy > 0) {
      label = busy === 1 ? "Downloading" : busy + " downloading";
      tone = "live";
    } else if (S.view === "compose" && S.composeState === "probing") {
      label = "Probing";
      tone = "busy";
    } else if (paused > 0) {
      label = paused === 1 ? "Paused" : paused + " paused";
      tone = "warn";
    } else if (S.view !== "compose" && selected()) {
      var s = selected().state;
      label = s === "done" ? "Complete" : s === "error" ? "Failed" : "Idle";
      tone = s === "done" ? "ok" : s === "error" ? "error" : "idle";
    } else {
      label = "Idle";
      tone = "idle";
    }
    els.statusText.textContent = label;
    els.statusPill.dataset.tone = tone;

    var active = activeCount();
    els.sidebarCount.textContent = active === 0 ? "" : active + " active";
    // Navigation is never gated: at the cap the user still needs to get back to
    // the compose form (and to a row, to free a slot). Only Start is disabled.
    els.newBtn.title = "";
    // The compose action bar depends on the active count (the cap disables Start
    // and explains why), so it has to follow the count even while the stage shows
    // a run - otherwise finishing a download leaves a stale "8 at once" lock.
    if (S.view === "compose") renderFooter();
  }

  // ---------- compose ----------

  function renderCompose() {
    var st = S.composeState;
    var probed = !!S.probe;

    els.readyCard.classList.toggle("is-empty", !probed);
    els.urlInput.disabled = st === "probing";
    els.probeBtn.disabled = st === "probing" || !els.urlInput.value.trim();
    els.chooseBtn.disabled = st === "probing";
    els.connRange.disabled = false;

    els.urlMsg.classList.toggle("is-busy", st === "probing");
    els.urlWrap.classList.toggle("is-invalid", !!S.composeError);
    els.urlMsg.hidden = st !== "probing" && !els.urlMsgText.textContent.trim();

    els.errorBanner.hidden = !S.composeError;
    if (S.composeError) els.errorMsg.textContent = S.composeError;

    els.destText.textContent = S.settings.destDir || "No folder chosen";
    els.destText.classList.toggle("is-empty", !S.settings.destDir);

    renderFooter();
  }

  function renderStepper() {
    var n = S.settings.connections;
    els.connRange.value = String(n);
    els.connValue.textContent = String(n);
    els.connRange.style.setProperty(
      "--fill",
      ((n - MIN_CONNECTIONS) / (MAX_CONNECTIONS - MIN_CONNECTIONS) * 100) + "%"
    );
    els.connRange.setAttribute("aria-valuetext", n + (n === 1 ? " connection" : " connections"));
  }

  function populateDetails(info) {
    els.filenameInput.value = info.filename || "";
    els.filenameInput.title = info.filename || "";
    els.metaSize.textContent = info.size == null ? "Unknown" : formatBytes(info.size);
    els.metaSize.style.color = info.size == null ? "var(--muted)" : "";
    els.metaRanges.textContent = info.acceptsRanges ? "Supported" : "Not supported";
    els.metaRanges.className = "badge " + (info.acceptsRanges ? "badge-yes" : "badge-no");
    els.metaProbe.textContent = info.probeMethod === "range" ? "Range GET" : "HEAD";
    els.metaServer.textContent = info.server || "Not advertised";
    els.metaServer.title = info.server || "";
    if (info.contentType) {
      els.typeChip.textContent = info.contentType;
      els.typeChip.title = info.contentType;
      els.typeChip.hidden = false;
    } else {
      els.typeChip.hidden = true;
    }
    var multi = !!info.acceptsRanges && info.size != null;
    els.connNote.hidden = multi;
    els.connNote.textContent = multi
      ? ""
      : info.size == null
        ? "Unknown file size, so one connection"
        : "This server ignores range requests, so one connection";
    els.resumeNote.hidden = true;
  }

  function currentFilename() {
    var v = els.filenameInput.value.trim();
    return v || (S.probe ? S.probe.filename : "");
  }

  function maybeProbe() {
    if (S.composeState === "probing") return;
    var url = els.urlInput.value.trim();
    if (!url) return;
    doProbe(url);
  }

  async function doProbe(url) {
    var seq = ++S.probeSeq;
    S.composeError = "";
    S.composeState = "probing";
    els.urlMsgText.textContent = "Checking the origin\u2026";
    renderCompose();
    renderChrome();
    try {
      var info = await invoke("probe_url", { url: url });
      if (seq !== S.probeSeq || els.urlInput.value.trim() !== url) return;
      if (!info || typeof info !== "object") throw new Error("empty probe response");
      S.probe = info;
      S.metaSeq++;
      populateDetails(info);
      els.urlMsgText.textContent = "";
      S.composeState = "ready";
      renderCompose();
      renderChrome();
      if (S.settings.destDir) metaCheck();
    } catch (err) {
      if (seq !== S.probeSeq) return;
      S.probe = null;
      els.urlMsgText.textContent = "";
      S.composeState = "idle";
      S.composeError = errText(err);
      renderCompose();
      renderChrome();
    }
  }

  async function metaCheck() {
    if (!S.probe || !S.settings.destDir || S.composeState !== "ready") {
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
      if (seq !== S.metaSeq || S.composeState !== "ready") return;
      var p = pctOf(res && res.bytesDone, S.probe.size);
      var show = !!(res && res.compatible && res.bytesDone > 0 && p != null && p > 0);
      els.resumeNote.hidden = !show;
      if (show) {
        els.resumeText.textContent =
          "Partial file found: " + formatBytes(res.bytesDone) + " of " +
          formatBytes(S.probe.size) + " done. Start will resume it.";
      }
    } catch (err) {
      if (seq === S.metaSeq) els.resumeNote.hidden = true;
    }
  }

  // ---------- starting a download ----------

  async function startDownload() {
    if (!S.probe || S.starting || !S.settings.destDir) return;
    if (activeCount() >= MAX_CONCURRENT) {
      S.composeError = "too many downloads in progress";
      renderCompose();
      return;
    }
    var args = {
      url: S.probe.url,
      destDir: S.settings.destDir,
      filename: currentFilename(),
      connections: S.settings.connections,
      size: S.probe.size,
      acceptsRanges: S.probe.acceptsRanges
    };
    S.starting = true;
    S.pending = args;
    S.adoptedId = null;
    S.composeError = "";
    renderCompose();
    els.primaryBtn.disabled = true;
    try {
      var res = await invoke("start_download", args);
      if (!res || !res.id) throw new Error("engine did not return an id");
      var adopted = S.adoptedId;
      S.starting = false;
      S.pending = null;
      S.adoptedId = null;
      // An event may have created the row already; keep the id the engine
      // actually returned and drop an adopted row that disagrees with it.
      if (adopted && adopted !== res.id) dropItem(adopted);
      var item = addItem(makeItem(res.id, args));
      setView(item.id);
      resetCompose();
    } catch (err) {
      var adoptedOnError = S.adoptedId;
      S.starting = false;
      S.pending = null;
      S.adoptedId = null;
      if (adoptedOnError && S.items[adoptedOnError]) {
        // The run is real even though the reply was lost.
        setView(adoptedOnError);
        resetCompose();
      } else if (adoptedOnError) {
        // Cancelled while the reply was in flight: nothing to show or report.
        setView("compose");
        resetCompose();
      } else {
        S.composeError = errText(err);
        renderCompose();
      }
    }
  }

  function resetCompose() {
    S.probe = null;
    S.probeSeq++;
    S.composeState = "idle";
    S.composeError = "";
    els.urlInput.value = "";
    els.urlMsgText.textContent = "Paste a URL and press Enter to check the origin.";
    els.resumeNote.hidden = true;
    els.readyCard.classList.add("is-empty");
    renderCompose();
    renderChrome();
  }

  // ---------- detail ----------

  function renderDetail(forceGrid) {
    var item = selected();
    if (!item) return;
    els.dlName.textContent = item.filename;
    els.dlName.title = item.filename;
    els.dlSub.textContent =
      (item.state === "done" ? formatBytes(item.doneSize || item.total) + " \u00b7 " : "") +
      item.destDir;
    els.dlSub.title = item.destDir;

    var live = item.state === "downloading" || item.state === "paused" || item.state === "error";
    var finished = item.state === "done";

    els.barTrack.hidden = !(live || finished);
    els.connSummaryRow.hidden = !live;
    if (finished) els.connGrid.hidden = true;
    els.stateNote.hidden = !item.note;
    if (item.note) els.stateNote.textContent = item.note;
    els.transferError.hidden = item.state !== "error";
    if (item.state === "error") els.transferErrorMsg.textContent = item.error || "download failed";
    els.donePanel.hidden = !finished;
    if (finished) {
      els.doneTitle.textContent = item.doneSize != null
        ? "Download complete \u00b7 " + formatBytes(item.doneSize)
        : "Download complete";
      els.donePath.textContent = item.path ? middleEllipsis(item.path, 54) : "";
      els.donePath.title = item.path || "";
      els.donePath.classList.remove("is-error");
    }

    applyNumbers(item);
    if (forceGrid) S.gridKey = "";
    applyConns(item);
    renderFooter();
  }

  function applyNumbers(item) {
    var known = item.total != null && item.total > 0;
    var overall = known ? pctOf(item.bytesDone, item.total) : null;
    els.barTrack.classList.toggle("is-indeterminate", !known && item.state === "downloading");

    if (item.state === "done") {
      els.barFill.style.width = "100%";
      els.progressPct.textContent = "100%";
      S.ariaPct = 100;
      els.barTrack.setAttribute("aria-valuenow", "100");
      els.barTrack.setAttribute("aria-valuetext", "100 percent, download complete");
    } else {
      if (known) els.barFill.style.width = (overall == null ? 0 : overall).toFixed(2) + "%";
      else els.barFill.style.width = "";

      var whole = known && overall != null ? Math.floor(overall) : 0;
      if (whole !== S.ariaPct) {
        S.ariaPct = whole;
        els.barTrack.setAttribute("aria-valuenow", String(whole));
        els.barTrack.setAttribute(
          "aria-valuetext",
          known
            ? formatBytes(item.bytesDone) + " of " + formatBytes(item.total) + ", " + whole + " percent"
            : formatBytes(item.bytesDone) + " downloaded"
        );
      }
      els.progressPct.textContent = known && overall != null
        ? (overall >= 100 ? "100" : overall.toFixed(1)) + "%"
        : formatBytes(item.bytesDone);
    }

    var stalled = item.state === "paused" || item.state === "error" || item.state === "done";
    els.statSpeed.textContent = stalled ? "\u2014" : formatSpeed(item.speedBps);
    els.statBytes.textContent = known
      ? formatBytes(item.bytesDone) + " / " + formatBytes(item.total)
      : formatBytes(item.bytesDone);
    els.statEta.textContent = stalled ? "\u2014" : formatEta(item.etaSecs);
  }

  function applyConns(item) {
    var conns = item.conns || [];
    S.liveConns = conns.length || item.connections;
    els.connGrid.hidden = S.liveConns <= 1 || item.state === "done";
    // Keyed on the run as well as the count: selecting a run that has not
    // reported a frame yet must clear the previous run's teeth, not inherit them.
    var key = item.id + ":" + conns.length;
    if (S.gridKey !== key || S.cells.length !== conns.length) {
      S.gridKey = key;
      buildConnGrid(conns.length);
    }
    if (conns.length === 0) {
      els.connSummary.textContent = "0/" + item.connections + " complete";
      return;
    }
    var doneCount = 0;
    for (var i = 0; i < conns.length; i++) {
      var c = conns[i];
      var cell = S.cells[i];
      if (!cell) break;
      var phase = c.phase || "idle";
      if (phase === "done") doneCount++;
      if (cell.root.dataset.phase !== phase) cell.root.dataset.phase = phase;
      var cp = pctOf(c.done, c.size);
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
      : doneCount + "/" + conns.length + " complete";
  }

  function buildConnGrid(n) {
    els.connGrid.textContent = "";
    S.cells = [];
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

  // ---------- footer ----------

  function show(el, on) { el.hidden = !on; }

  function renderFooter() {
    var hint = "";
    var tone = "";
    var item = selected();

    if (S.view === "compose") {
      show(els.cancelBtn, false);
      show(els.pauseBtn, false);
      show(els.removeBtn, false);
      show(els.revealBtn, false);
      show(els.primaryBtn, true);
      var atCap = activeCount() >= MAX_CONCURRENT;
      els.primaryBtn.textContent = "Start download";
      els.primaryBtn.disabled = !S.probe || !S.settings.destDir || S.starting ||
        S.composeState === "probing" || atCap;
      if (atCap) {
        hint = "Up to " + MAX_CONCURRENT + " at once - cancel one to start another";
        tone = "warn";
      } else if (S.composeState === "probing") {
        hint = "";
      } else if (S.probe && !S.settings.destDir) {
        hint = "Choose a destination folder to continue";
        tone = "warn";
      } else if (S.probe) {
        var n = S.settings.connections;
        hint = n + (n === 1 ? " connection" : " connections");
      }
    } else if (item) {
      show(els.primaryBtn, item.state === "error");
      show(els.removeBtn, item.state === "done" || item.state === "error");
      show(els.revealBtn, item.state === "done");
      show(els.cancelBtn, item.state === "downloading" || item.state === "paused");
      show(els.pauseBtn, item.state === "downloading" || item.state === "paused");

      if (item.state === "downloading") {
        hint = "Streaming " + (item.conns.length || item.connections) +
          ((item.conns.length || item.connections) === 1 ? " connection" : " connections");
      } else if (item.state === "paused") {
        hint = "Paused, offsets saved";
        tone = "warn";
      } else if (item.state === "done") {
        hint = item.destDir ? "Saved to " + middleEllipsis(item.destDir, 34) : "Saved";
      } else if (item.state === "error") {
        hint = "Download failed";
        tone = "error";
      }

      els.pauseBtn.textContent = item.state === "paused" ? "Resume" : "Pause";
      els.pauseBtn.className = item.state === "paused" ? "btn btn-primary" : "btn btn-secondary";
      els.primaryBtn.textContent = "Try again";
      els.primaryBtn.disabled = false;
    }

    els.actionHint.textContent = hint;
    els.actionHint.hidden = !hint;
    if (tone) els.actionHint.dataset.tone = tone;
    else els.actionHint.removeAttribute("data-tone");
  }

  // ---------- events ----------

  function onState(p) {
    if (!p) return;
    // A terminal event is how a run leaves the list, so it is never stale:
    // suppressing it while a cancel is in flight would both strand the row and
    // turn the engine's "already finished" reply into a false failure.
    var terminal = p.state === "done" || p.state === "error";
    if (p.id != null && S.deadIds[p.id] && !terminal) return;

    var item = p.id != null ? S.items[p.id] : null;
    if (!item && p.id != null && S.starting && S.pending && !S.adoptedId) {
      // An event beat the start ack. Build the row from the request we actually
      // sent - not from the event - or a later retry inherits an empty url, an
      // unknown size and no range answer. The ack reconciles it by id.
      S.adoptedId = p.id;
      item = addItem(makeItem(p.id, S.pending));
      item.state = p.state;
      setView(p.id);
      resetCompose();
    }
    if (!item) return;

    // done/error are final for an id: a frame that was already in flight must not
    // resurrect a settled row (which is what happens when a cancel loses its race
    // and the retried id stops being suppressed).
    if (!terminal && (item.state === "done" || item.state === "error")) return;

    if (p.state === "downloading") item.note = p.note || "";
    if (p.state === "paused") item.note = "Paused, offsets saved";
    if (p.state === "done") {
      item.note = "";
      item.state = "done";
      item.path = typeof p.path === "string" ? p.path : null;
      item.doneSize = p.size != null ? p.size : item.total;
      item.speedBps = 0;
      item.etaSecs = null;
      if (item.total != null) item.bytesDone = item.total;
    } else if (p.state === "error") {
      item.note = "";
      item.state = "error";
      item.error = p.message || "download failed";
      item.speedBps = 0;
    } else {
      item.state = p.state;
    }

    S.dirty[item.id] = true;
    scheduleFlush();
    if (S.view === item.id) renderDetail(true);
    renderChrome();
  }

  function onProgress(p) {
    if (!p) return;
    if (p.id != null && S.deadIds[p.id]) return;
    var item = S.items[p.id];
    if (!item) return;
    item.bytesDone = p.bytesDone || 0;
    item.total = p.total != null ? p.total : item.total;
    item.speedBps = p.speedBps || 0;
    item.etaSecs = p.etaSecs != null ? p.etaSecs : null;
    item.conns = Array.isArray(p.conns) ? p.conns : [];
    S.dirty[item.id] = true;
    scheduleFlush();
  }

  function scheduleFlush() {
    if (S.rafPending) return;
    S.rafPending = true;
    requestAnimationFrame(flush);
  }

  function flush() {
    S.rafPending = false;
    var ids = Object.keys(S.dirty);
    S.dirty = {};
    for (var i = 0; i < ids.length; i++) {
      updateRow(ids[i]);
      if (S.view === ids[i]) {
        var item = S.items[ids[i]];
        if (item) {
          applyNumbers(item);
          applyConns(item);
        }
      }
    }
  }

  // ---------- actions ----------

  async function closeItem(id) {
    var item = S.items[id];
    if (!item) return;
    if (item.state === "downloading" || item.state === "paused") {
      await cancelItem(item);
    } else {
      dropItem(id);
    }
  }

  async function cancelItem(item) {
    if (S.controlBusy[item.id]) return;
    S.controlBusy[item.id] = true;
    S.deadIds[item.id] = 1;
    try {
      await invoke("cancel_download", { id: item.id });
      dropItem(item.id);
    } catch (err) {
      // The engine answers "the download had already finished" when the run
      // settled while we were asking. That is not a failure of the run, so a
      // completed item stays completed; anything else surfaces the verbatim
      // reason. Late events are re-admitted either way, so a dropped terminal
      // event can still land.
      S.deadIds[item.id] = 0;
      if (item.state !== "done") {
        item.state = "error";
        item.error = errText(err);
        S.dirty[item.id] = true;
        scheduleFlush();
        if (S.view === item.id) renderDetail(true);
      }
      renderChrome();
    } finally {
      delete S.controlBusy[item.id];
    }
  }

  async function togglePause(item) {
    if (S.controlBusy[item.id]) return;
    if (item.state !== "downloading" && item.state !== "paused") return;
    S.controlBusy[item.id] = true;
    els.pauseBtn.disabled = true;
    try {
      if (item.state === "paused") await invoke("resume_download", { id: item.id });
      else await invoke("pause_download", { id: item.id });
    } catch (err) {
      // A pause can fail for real (the flush failed), so the row has to say so
      // instead of keeping a stale "downloading" that still counts toward the cap.
      // A run that finished while the command was in flight is not a failure.
      if (item.state === "done") {
        delete S.controlBusy[item.id];
        els.pauseBtn.disabled = false;
        renderChrome();
        return;
      }
      item.state = "error";
      item.error = errText(err);
      item.speedBps = 0;
      item.etaSecs = null;
      S.dirty[item.id] = true;
      scheduleFlush();
      if (S.view === item.id) renderDetail(true);
      renderChrome();
    } finally {
      delete S.controlBusy[item.id];
      els.pauseBtn.disabled = false;
    }
  }

  async function retry(item) {
    if (S.starting) return;
    if (activeCount() >= MAX_CONCURRENT) {
      item.error = "too many downloads in progress";
      renderDetail(true);
      return;
    }
    var args = {
      url: item.url,
      destDir: item.destDir,
      filename: item.filename,
      connections: item.connections,
      size: item.size,
      // Reuse what the probe actually found: claiming range support an origin
      // refused would put a retry into a mode that origin cannot serve.
      acceptsRanges: item.acceptsRanges !== false
    };
    S.starting = true;
    S.pending = args;
    S.adoptedId = null;
    try {
      var res = await invoke("start_download", args);
      if (!res || !res.id) throw new Error("engine did not return an id");
      var adopted = S.adoptedId;
      S.starting = false;
      S.pending = null;
      S.adoptedId = null;
      if (adopted && adopted !== res.id) dropItem(adopted);
      if (S.items[item.id]) dropItem(item.id);
      var fresh = addItem(makeItem(res.id, args));
      setView(fresh.id);
    } catch (err) {
      var adoptedOnError = S.adoptedId;
      S.starting = false;
      S.pending = null;
      S.adoptedId = null;
      if (!adoptedOnError) {
        item.state = "error";
        item.error = errText(err);
        renderDetail(true);
      } else if (S.items[adoptedOnError]) {
        setView(adoptedOnError);
      }
    }
    renderChrome();
  }

  // ---------- wiring ----------

  els.newBtn.addEventListener("click", function () {
    setView("compose");
    resetCompose();
    els.urlInput.focus();
  });

  els.urlInput.addEventListener("paste", function () { setTimeout(maybeProbe, 0); });
  els.urlInput.addEventListener("keydown", function (e) {
    if (e.key === "Enter") { e.preventDefault(); maybeProbe(); }
  });
  els.urlInput.addEventListener("input", function () {
    S.probeSeq++;
    S.composeError = "";
    if (S.probe) {
      S.probe = null;
      S.composeState = "idle";
      els.resumeNote.hidden = true;
      els.readyCard.classList.add("is-empty");
    }
    renderCompose();
    renderChrome();
  });
  els.probeBtn.addEventListener("click", function () { maybeProbe(); });

  var metaTimer = 0;
  els.filenameInput.addEventListener("input", function () {
    if (S.composeState !== "ready") return;
    clearTimeout(metaTimer);
    metaTimer = setTimeout(function () {
      if (S.composeState === "ready" && S.settings.destDir) metaCheck();
    }, 350);
  });
  els.filenameInput.addEventListener("keydown", function (e) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (!els.primaryBtn.disabled) startDownload();
    }
  });

  els.chooseBtn.addEventListener("click", async function () {
    try {
      var dir = await invoke("pick_folder", {});
      if (dir == null) return;
      S.settings.destDir = dir;
      renderCompose();
      invoke("save_settings", { destDir: dir }).catch(function () {});
      metaCheck();
    } catch (err) {
      S.composeError = errText(err);
      renderCompose();
    }
  });

  els.connRange.addEventListener("input", function () {
    var n = clampConnections(els.connRange.value);
    S.settings.connections = n;
    renderStepper();
    renderCompose();
    invoke("save_settings", { connections: n }).catch(function () {});
  });

  els.primaryBtn.addEventListener("click", function () {
    var item = selected();
    if (S.view !== "compose" && item && item.state === "error") {
      retry(item);
      return;
    }
    if (S.view !== "compose") return;
    if (!S.settings.destDir) {
      S.composeError = "";
      els.chooseBtn.focus();
      els.actionHint.textContent = "Choose a destination folder to continue";
      els.actionHint.hidden = false;
      els.actionHint.dataset.tone = "warn";
      return;
    }
    startDownload();
  });

  els.cancelBtn.addEventListener("click", function () {
    var item = selected();
    if (item) cancelItem(item);
  });

  els.pauseBtn.addEventListener("click", function () {
    var item = selected();
    if (item) togglePause(item);
  });

  els.removeBtn.addEventListener("click", function () {
    var item = selected();
    if (item) dropItem(item.id);
  });

  els.revealBtn.addEventListener("click", async function () {
    var item = selected();
    if (!item || !item.path) return;
    try {
      await invoke("reveal_path", { path: item.path });
    } catch (err) {
      els.donePath.textContent = errText(err);
      els.donePath.classList.add("is-error");
    }
  });

  document.addEventListener("keydown", function (e) {
    if (e.key !== "Escape") return;
    if (S.view !== "compose") setView("compose");
  });

  // ---------- boot ----------

  async function init() {
    var backend = false;
    try {
      S.unlisten.push(await window.__TAURI__.event.listen("hydra://state", function (e) {
        onState(e && e.payload);
      }));
      S.unlisten.push(await window.__TAURI__.event.listen("hydra://progress", function (e) {
        onProgress(e && e.payload);
      }));
      window.addEventListener("beforeunload", function () {
        S.unlisten.forEach(function (off) { try { off(); } catch (err) { /* going away */ } });
        S.unlisten = [];
      });
      backend = true;
    } catch (err) { /* no event channel */ }

    try {
      var s = await invoke("get_settings", {});
      if (s && typeof s === "object") {
        if (typeof s.destDir === "string") S.settings.destDir = s.destDir;
        if (typeof s.connections === "number") S.settings.connections = clampConnections(s.connections);
      }
      backend = true;
    } catch (err) { /* defaults stand */ }

    renderStepper();
    rebuildList();
    setView("compose", false);
    els.probeBtn.disabled = true;
    // The compose flow starts at the field the user must fill.
    if (!document.body.dataset.view || document.body.dataset.view === "compose") {
      els.urlInput.focus();
    }

    if (backend) {
      els.engineBadge.hidden = false;
      setTimeout(function () { els.engineBadge.hidden = true; }, 2400);
    }
  }

  init();
})();
