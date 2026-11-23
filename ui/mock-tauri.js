/* mock-tauri.js — dev-only Tauri shim.
 *
 * Installs window.__TAURI__ only when the real runtime has not already done so,
 * which makes this file inert inside the built app.
 *
 *   ?demo=idle|probing|ready|resume|downloading|paused|done|error|single
 *   ?snap=1   resolve and settle instantly (deterministic screenshots)
 */

(function () {
  if (window.__TAURI__) return; // real Tauri: do nothing
  if (!/^(https?|file):$/.test(location.protocol)) return;

  var TOTAL = 7772412965;  // ~7.24 GiB, synthetic
  var SPEED = 38170000;    // ~36.4 MiB/s, synthetic
  var FILENAME = "sample-dataset.tar.zst";

  var listeners = {};
  var settings = { destDir: "/Users/you/Downloads", connections: 8 };
  var demo = (location.search.match(/[?&]demo=([a-z]+)/) || [])[1] || "";
  var SNAP = /[?&]snap=1/.test(location.search);
  var paused = false;
  var running = null;
  var conns = [];

  function emit(name, payload) {
    (listeners[name] || []).forEach(function (cb) {
      try { cb({ event: name, id: 0, payload: payload }); } catch (e) { console.error(e); }
    });
  }

  function plan(n) {
    var size = Math.ceil(TOTAL / n);
    var out = [];
    for (var i = 0; i < n; i++) {
      out.push({
        index: i,
        size: Math.min(size, TOTAL - i * size),
        done: 0,
        active: true,
        phase: "connecting"
      });
    }
    return out;
  }

  function snapshot() {
    var done = 0;
    conns.forEach(function (c) { done += c.done; });
    return {
      id: "dl-mock",
      bytesDone: done,
      total: TOTAL,
      speedBps: paused ? 0 : SPEED,
      etaSecs: paused ? null : Math.max(0, Math.round((TOTAL - done) / SPEED)),
      conns: conns.map(function (c) {
        return { index: c.index, done: c.done, size: c.size, active: c.active, phase: c.phase };
      })
    };
  }

  function setRun(fraction, opts) {
    opts = opts || {};
    var n = opts.conns || 8;
    conns = plan(n);
    var per = (TOTAL * fraction) / n;
    conns.forEach(function (c, i) {
      c.done = Math.min(c.size, per);
      c.phase = c.done >= c.size ? "done" : (opts.retryIndex === i ? "retrying" : "receiving");
      c.active = c.phase !== "done";
    });
    paused = false;
    emit("hydra://state", { id: "dl-mock", state: "downloading", note: opts.note });
    emit("hydra://progress", snapshot());
  }

  function setPaused() {
    paused = true;
    emit("hydra://state", { id: "dl-mock", state: "paused" });
  }

  function setDone() {
    conns.forEach(function (c) { c.done = c.size; c.phase = "done"; c.active = false; });
    paused = true;
    emit("hydra://state", { id: "dl-mock", state: "downloading" });
    emit("hydra://progress", snapshot());
    emit("hydra://state", {
      id: "dl-mock",
      state: "done",
      path: settings.destDir + "/" + FILENAME,
      size: TOTAL
    });
  }

  function setError(message) {
    paused = true;
    emit("hydra://state", { id: "dl-mock", state: "error", message: message });
  }

  function tick() {
    var allDone = true;
    var per = (SPEED * 0.25) / conns.length;
    conns.forEach(function (c) {
      if (c.done < c.size) {
        c.done = Math.min(c.size, c.done + per);
        c.phase = "receiving";
        c.active = true;
        allDone = false;
      } else {
        c.phase = "done";
        c.active = false;
      }
    });
    emit("hydra://progress", snapshot());
    if (allDone) { stop(); setDone(); }
  }

  function stop() {
    if (running) { clearInterval(running); running = null; }
  }

  var probeResult = {
    url: "https://origin.example/files/" + FILENAME,
    size: TOTAL,
    acceptsRanges: true,
    filename: FILENAME,
    contentType: "application/zstd",
    probeMethod: "head",
    server: "nginx/1.25.3"
  };

  function invoke(name, args) {
    args = args || {};
    return new Promise(function (resolve, reject) {
      var delay = SNAP ? 0 : (name === "probe_url" ? 420 : 40);
      setTimeout(function () {
        switch (name) {
          case "get_settings":
            resolve({ destDir: settings.destDir, connections: settings.connections });
            return;
          case "save_settings":
            if (typeof args.destDir === "string") settings.destDir = args.destDir;
            if (typeof args.connections === "number") settings.connections = args.connections;
            resolve(null);
            return;
          case "probe_url":
            resolve(JSON.parse(JSON.stringify(probeResult)));
            return;
          case "meta_check":
            if (demo === "resume") {
              resolve({ exists: true, compatible: true, bytesDone: Math.round(TOTAL * 0.2516) });
            } else {
              resolve({ exists: false, compatible: false, bytesDone: 0 });
            }
            return;
          case "pick_folder":
            settings.destDir = "/Users/you/Movies/Hydra";
            resolve(settings.destDir);
            return;
          case "start_download":
            resolve({ id: "dl-mock" });
            return;
          case "pause_download":
            setPaused();
            resolve(null);
            return;
          case "resume_download":
            paused = false;
            emit("hydra://state", { id: "dl-mock", state: "downloading" });
            resolve(null);
            return;
          case "cancel_download":
            stop();
            resolve(null);
            return;
          case "reveal_path":
          case "hydra_ping":
            resolve(null);
            return;
          default:
            reject("unknown command: " + name);
        }
      }, delay);
    });
  }

  window.__TAURI__ = {
    core: { invoke: invoke },
    event: {
      listen: function (name, cb) {
        (listeners[name] = listeners[name] || []).push(cb);
        return Promise.resolve(function () {});
      }
    }
  };

  // ---------- demo driver ----------

  window.addEventListener("load", function () {
    if (!demo || demo === "idle") return;

    var input = document.getElementById("url-input");
    input.value = probeResult.url;
    input.dispatchEvent(new Event("input"));
    document.getElementById("probe-btn").click();

    if (demo === "probing") return;

    setTimeout(function () {
      if (demo === "ready" || demo === "resume") return;

      if (demo === "downloading") {
        setRun(0.42);
        if (!SNAP) running = setInterval(tick, 250);
      } else if (demo === "paused") {
        setRun(0.42);
        setPaused();
      } else if (demo === "single") {
        setRun(0.31, { conns: 1 });
      } else if (demo === "done") {
        conns = plan(8);
        setDone();
      } else if (demo === "error") {
        setRun(0.3, { retryIndex: 3 });
        setError("connection lost mid-download (after 5 retries)");
      }
    }, SNAP ? 60 : 700);
  });
})();
