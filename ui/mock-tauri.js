/* mock-tauri.js - Hydra dev harness.
 *
 * Loaded by index.html BEFORE main.js. Inside a real Tauri WKWebView
 * (window.__TAURI_INTERNALS__ present) it does NOTHING. In a plain browser it
 * installs a fake window.__TAURI__ (core.invoke + event.listen per CONTRACT.md)
 * and drives a simulated download so the UI can be developed without the Rust
 * backend.
 *
 * Trigger words inside the pasted URL force visual states:
 *   "403"         probe rejects with "HTTP 403"
 *   "fail"        state error mid-download: "connection lost mid-download (after 5 retries)"
 *   "noranges"    probe returns acceptsRanges false (single stream only)
 *   "unknown"     probe returns size null (unknown size, indeterminate UI)
 *   "resume"     meta_check compatible, bytesDone = 20.3% of size;
 *                 start_download emits note "Resuming partial download"
 *   "fall"        start_download emits note "server refused range requests - using a single connection",
 *                 then single-stream progress
 *   "cancelfolder" pick_folder returns null
 *   "slow"        probe takes 30 s (lets you look at the probing state)
 *
 * Dev capture helper: open index.html?demo=<state> (ready | probing | downloading |
 * paused | done | error | unknown | noranges | resume | single | folderprompt)
 * and the harness drives the real UI through genuine DOM events.
 */

"use strict";

(function () {
  if (window.__TAURI_INTERNALS__) return; // real Tauri: stand aside
  if (window.__TAURI__) return;

  var SIZE = 7772412965; // 7.24 GiB
  var FILENAME = "reference-file.mkv";
  var CONTENT_TYPE = "application/octet-stream";
  var MIB = 1048576;

  var PROBE_MS = 650;  // probe latency
  var TICK_MS = 160;   // progress at ~6 Hz (contract floor is 4 Hz)
  var TIME_SCALE = 12; // dev compression: bytes advance 12x faster than the displayed speed implies
  var UNKNOWN_TOTAL = Math.round(3.35 * 1024 * 1024 * 1024); // run length when size is unknown

  var listeners = new Map();

  function emit(name, payload) {
    var set = listeners.get(name);
    if (!set) return;
    var snapshot = Array.from(set);
    for (var i = 0; i < snapshot.length; i++) snapshot[i]({ payload: payload });
  }

  function listen(name, cb) {
    var set = listeners.get(name);
    if (!set) listeners.set(name, (set = new Set()));
    set.add(cb);
    return Promise.resolve(function () { set.delete(cb); });
  }

  function delay(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }

  // ----- settings (in-memory stand-in for settings.json) -----

  var settings = { destDir: null, connections: 8 };
  var lastUrl = "";

  // ----- download simulation -----

  var sim = null;
  var simSeq = 0;
  var demoScale = 0;   // autodrive: per-demo time scale (0 = default)
  var demoPauseAt = 0; // autodrive: pause when overall fraction crosses this
  var demoFailAt = 0;  // autodrive: fail when sim time passes this (sim seconds)

  function stopTimer(s) {
    if (s && s.timer) { clearInterval(s.timer); s.timer = 0; }
  }

  function bytesDone(s) {
    var n = 0;
    for (var i = 0; i < s.conns.length; i++) n += s.conns[i].done;
    return n;
  }

  function makeSim(args) {
    var url = String(args.url || "");
    var total = args.size == null ? null : Number(args.size);
    var single = !args.acceptsRanges || total == null || url.indexOf("fall") !== -1;
    var resume = url.indexOf("resume") !== -1 && total != null;
    var n = single ? 1 : Math.min(16, Math.max(1, Number(args.connections) || 8));
    var chunk = total != null ? Math.ceil(total / n) : null;
    var conns = [];
    for (var i = 0; i < n; i++) {
      var size = total != null ? (i === n - 1 ? Math.max(0, total - i * chunk) : chunk) : null;
      var done = resume ? Math.min(size || 0, Math.round((size || 0) * 0.203)) : 0;
      conns.push({ done: done, size: size, phase: "connecting", active: true });
    }
    var note = null;
    if (url.indexOf("fall") !== -1) note = "server refused range requests - using a single connection";
    else if (resume) note = "Resuming partial download";
    return {
      id: "dl-" + (++simSeq),
      url: url,
      destDir: String(args.destDir || ""),
      filename: String(args.filename || FILENAME),
      single: single,
      total: total,
      runTotal: total != null ? total : UNKNOWN_TOTAL,
      conns: conns,
      t: 0,
      scale: demoScale || TIME_SCALE,
      pauseAt: demoPauseAt,
      pauseClicked: false,
      startBytes: resume ? conns.reduce(function (a, c) { return a + c.done; }, 0) : 0,
      willFail: url.indexOf("fail") !== -1,
      failAt: demoFailAt || 55,
      retried: false,
      retryIndex: n > 1 ? 1 : 0,
      retryUntil: 0,
      dip1: false,
      dip2: false,
      dipUntil: 0,
      dipMul: 1,
      timer: 0,
      paused: false,
      note: note
    };
  }

  function tick() {
    var s = sim;
    if (!s || s.paused) return;

    var dt = (TICK_MS / 1000) * (s.scale || TIME_SCALE);
    s.t += dt;

    if (s.willFail && s.t > s.failAt) {
      stopTimer(s);
      emit("hydra://state", { id: s.id, state: "error", message: "connection lost mid-download (after 5 retries)" });
      sim = null;
      return;
    }

    var doneNow = bytesDone(s);
    var span = Math.max(1, s.runTotal - s.startBytes);
    var frac = Math.min(1, Math.max(0, (doneNow - s.startBytes) / span));

    if (s.pauseAt && !s.pauseClicked && frac >= s.pauseAt) {
      s.pauseClicked = true;
      var pb = document.getElementById("pause-btn");
      if (pb && !pb.hidden) pb.click();
    }

    // speed model, displayed bytes/s (multi-stream sustains ~26 MiB/s, single swings)
    var cruise = s.single ? 13.2 : 26.4;
    var rampT = Math.min(1, s.t / 60);
    var mib = 3.5 + (cruise - 3.5) * (rampT * rampT * (3 - 2 * rampT));
    if (!s.dip1 && frac > 0.42) { s.dip1 = true; s.dipUntil = s.t + 14; s.dipMul = 0.55; }
    if (!s.dip2 && frac > 0.74) { s.dip2 = true; s.dipUntil = s.t + 10; s.dipMul = 0.62; }
    if (s.dipUntil && s.t < s.dipUntil) mib *= s.dipMul;
    mib *= 0.88 + Math.random() * 0.24;
    if (s.single) mib *= 0.75 + Math.random() * 0.5;
    var speed = Math.max(0.4, mib) * MIB;

    // phases
    var connecting = s.t < 5;
    if (!s.retried && frac > 0.3 && s.conns.length > 1) { s.retried = true; s.retryUntil = s.t + 12; }
    var inRetry = s.t < s.retryUntil;

    var activeIdx = [];
    for (var i = 0; i < s.conns.length; i++) {
      var c = s.conns[i];
      if (c.size != null && c.done >= c.size) { c.phase = "done"; c.active = false; continue; }
      if (connecting) { c.phase = "connecting"; c.active = true; continue; }
      if (i === s.retryIndex && inRetry) { c.phase = "retrying"; c.active = true; continue; }
      c.phase = "receiving";
      c.active = true;
      activeIdx.push(i);
    }

    if (connecting) speed = 0; // nothing received yet
    else if (activeIdx.length) {
      for (var k = 0; k < activeIdx.length; k++) {
        var ci = s.conns[activeIdx[k]];
        ci.done += (speed * dt) / activeIdx.length * (0.9 + ((activeIdx[k] * 37) % 21) / 100);
        if (ci.size != null && ci.done > ci.size) ci.done = ci.size;
      }
    }

    doneNow = bytesDone(s);
    var complete = doneNow >= s.runTotal;

    if (complete) {
      for (var j = 0; j < s.conns.length; j++) {
        if (s.conns[j].size != null) s.conns[j].done = s.conns[j].size;
        s.conns[j].phase = "done";
        s.conns[j].active = false;
      }
      doneNow = bytesDone(s);
      stopTimer(s);
      emit("hydra://progress", {
        id: s.id,
        bytesDone: doneNow,
        total: s.total,
        speedBps: Math.round(speed),
        etaSecs: 0,
        conns: connsPayload(s)
      });
      var verified = s.total != null ? s.total : doneNow;
      emit("hydra://state", {
        id: s.id,
        state: "done",
        path: (s.destDir ? s.destDir.replace(/\/+$/, "") + "/" : "") + s.filename,
        size: verified
      });
      sim = null;
      return;
    }

    var eta = null;
    if (s.total != null && speed > 1000) eta = Math.max(0, Math.round((s.total - doneNow) / speed));

    emit("hydra://progress", {
      id: s.id,
      bytesDone: Math.round(doneNow),
      total: s.total,
      speedBps: Math.round(speed),
      etaSecs: eta,
      conns: connsPayload(s)
    });
  }

  function connsPayload(s) {
    var out = [];
    for (var i = 0; i < s.conns.length; i++) {
      var c = s.conns[i];
      out.push({
        index: i,
        done: Math.round(c.done),
        size: c.size,
        active: c.active,
        phase: c.phase
      });
    }
    return out;
  }

  // ----- commands (names + args + return shapes per CONTRACT.md) -----

  var commands = {
    probe_url: function (args) {
      var url = String(args.url || "");
      lastUrl = url;
      var wait = url.indexOf("slow") !== -1 ? 30000 : PROBE_MS;
      return delay(wait).then(function () {
        if (url.indexOf("403") !== -1) throw "HTTP 403";
        var unknown = url.indexOf("unknown") !== -1;
        var noRanges = url.indexOf("noranges") !== -1;
        return {
          url: url, // final resolved URL
          size: unknown ? null : SIZE,
          acceptsRanges: !noRanges,
          filename: FILENAME,
          contentType: CONTENT_TYPE,
          probeMethod: noRanges ? "range" : "head"
        };
      });
    },

    start_download: function (args) {
      if (sim) { stopTimer(sim); sim = null; }
      var s = makeSim(args);
      sim = s;
      setTimeout(function () {
        if (sim !== s) return;
        var payload = { id: s.id, state: "downloading" };
        if (s.note) payload.note = s.note;
        emit("hydra://state", payload);
        s.timer = setInterval(tick, TICK_MS);
      }, 150);
      return { id: s.id };
    },

    pause_download: function (args) {
      var s = sim;
      if (s && s.id === args.id && !s.paused) {
        s.paused = true;
        stopTimer(s);
        emit("hydra://state", { id: s.id, state: "paused" });
      }
      return null;
    },

    resume_download: function (args) {
      var s = sim;
      if (s && s.id === args.id && s.paused) {
        s.paused = false;
        emit("hydra://state", { id: s.id, state: "downloading" });
        s.timer = setInterval(tick, TICK_MS);
      }
      return null;
    },

    cancel_download: function (args) {
      // contract: no state event afterwards; the UI resets itself
      var s = sim;
      if (s && s.id === args.id) { stopTimer(s); sim = null; }
      return null;
    },

    pick_folder: function () {
      return delay(250).then(function () {
        return String(lastUrl).indexOf("cancelfolder") !== -1 ? null : "/Users/you/Downloads";
      });
    },

    reveal_path: function () {
      return delay(120).then(function () { return null; });
    },

    get_settings: function () {
      return { destDir: settings.destDir, connections: settings.connections };
    },

    save_settings: function (args) {
      if ("destDir" in args) settings.destDir = args.destDir == null ? null : String(args.destDir);
      if (typeof args.connections === "number") settings.connections = args.connections;
      return null;
    },

    meta_check: function (args) {
      return delay(90).then(function () {
        var has = String(args.url || "").indexOf("resume") !== -1;
        var size = args.size == null ? null : Number(args.size);
        return {
          exists: has,
          compatible: has,
          bytesDone: has && size ? Math.round(size * 0.203) : 0
        };
      });
    }
  };

  function invoke(cmd, args) {
    return new Promise(function (resolve, reject) {
      setTimeout(function () {
        var fn = commands[cmd];
        if (typeof fn !== "function") {
          reject("mock-tauri: unknown command \"" + cmd + "\"");
          return;
        }
        Promise.resolve(fn(args || {})).then(resolve, reject);
      }, 0);
    });
  }

  window.__TAURI__ = {
    core: { invoke: invoke },
    event: { listen: listen }
  };

  // ----- dev capture helper (?demo=<state>): drives the real UI through DOM events -----

  var DEMOS = {
    ready: { url: "https://mirror.example/files/big-episode.mkv", start: false },
    probing: { url: "https://mirror.example/files/slow-probe.mkv", start: false },
    folderprompt: { url: "https://mirror.example/files/cancelfolder-pick.mkv", start: true, skipFolder: true },
    noranges: { url: "https://mirror.example/files/noranges-server.mkv", start: false },
    unknownsize: { url: "https://mirror.example/files/unknown-size.bin", start: false },
    resumeready: { url: "https://mirror.example/files/resume-partial.mkv", start: false },
    downloading: { url: "https://mirror.example/files/big-episode.mkv", start: true },
    paused: { url: "https://mirror.example/files/big-episode.mkv", start: true, scale: 60, pauseAt: 0.3 },
    done: { url: "https://mirror.example/files/big-episode.mkv", start: true, scale: 600 },
    error: { url: "https://mirror.example/files/fail-transfer.mkv", start: true, scale: 30, failAt: 30 },
    unknown: { url: "https://mirror.example/files/unknown-size.bin", start: true },
    single: { url: "https://mirror.example/files/fall-back-single.mkv", start: true },
    resume: { url: "https://mirror.example/files/resume-partial.mkv", start: true },
    probe403: { url: "https://mirror.example/files/403-forbidden.mkv", start: false }
  };

  function autodrive() {
    // dev capture timing: hold the window's load event for ?hold=<ms> so a
    // headless --dump-dom (which waits for load) captures a settled state
    try {
      var holdMs = Number(new URLSearchParams(window.location.search).get("hold")) || 0;
      if (holdMs > 0) {
        var holdImg = document.createElement("img");
        holdImg.src = "http://127.0.0.1:8802/hold?ms=" + holdMs;
        holdImg.width = 1;
        holdImg.height = 1;
        holdImg.alt = "";
        holdImg.style.display = "none";
        document.body.appendChild(holdImg);
      }
    } catch (eHold) { /* capture timing is best-effort */ }

    var demo = null;
    try {
      var key = new URLSearchParams(window.location.search).get("demo");
      demo = key ? DEMOS[key] || null : null;
    } catch (e) { return; }
    if (!demo) return;
    demoScale = demo.scale || 0;
    demoPauseAt = demo.pauseAt || 0;
    demoFailAt = demo.failAt || 0;
    // Run immediately: the elements above the scripts are already parsed, and a
    // headless --dump-dom captures AT the (held) load event - waiting for load
    // would put the whole flow after the capture.
    setTimeout(function () {
      var input = document.getElementById("url-input");
      if (!input) return;
      input.value = demo.url;
      input.dispatchEvent(new Event("paste"));
      var t0 = Date.now();
      var poll = setInterval(function () {
        var btn = document.getElementById("download-btn");
        var card = document.getElementById("ready-card");
        if (btn && card && !card.hidden) {
          clearInterval(poll);
          var afterFolder = function () {
            if (demo.start) btn.click();
          };
          var choose = document.getElementById("choose-btn");
          if (demo.skipFolder || !choose) {
            afterFolder();
          } else {
            // pick_folder first so "Download" is armed, then start
            choose.click();
            setTimeout(afterFolder, 700);
          }
        } else if (Date.now() - t0 > 15000) {
          clearInterval(poll);
        }
      }, 120);
    }, 80);
  }

  autodrive();
})();
