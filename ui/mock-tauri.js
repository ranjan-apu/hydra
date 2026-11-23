/* mock-tauri.js — dev-only Tauri shim.

 * Installs window.__TAURI__ only when the real runtime has not already done so,
 * which makes this file inert inside the built app.

 *   ?demo=compose|ready|multi      (default: a blank compose form)
 *   ?snap=1                        resolve instantly (deterministic screenshots)

 * The multi demo drives the REAL ui code path: it types a URL, probes, starts,
 * and then moves the runs into other states — nothing is faked at the DOM level.
 */

(function () {
  if (window.__TAURI__) return; // real Tauri: do nothing
  if (!/^(https?|file):$/.test(location.protocol)) return;

  var settings = { destDir: "/Users/you/Downloads", connections: 8 };
  var listeners = {};
  var runs = {};          // id -> run
  var nextId = 1;
  var demo = (location.search.match(/[?&]demo=([a-z]+)/) || [])[1] || "";
  var SNAP = /[?&]snap=1/.test(location.search);
  var queue = [];         // scripted targets, one per start_download

  function emit(name, payload) {
    (listeners[name] || []).forEach(function (cb) {
      try { cb({ event: name, id: 0, payload: payload }); } catch (e) { console.error(e); }
    });
  }

  function plan(n, total) {
    var size = Math.ceil(total / n);
    var out = [];
    for (var i = 0; i < n; i++) out.push({ index: i, size: Math.min(size, total - i * size), done: 0, active: true, phase: "connecting" });
    return out;
  }

  function snapshot(run) {
    var done = 0;
    run.conns.forEach(function (c) { done += c.done; });
    return {
      id: run.id,
      bytesDone: done,
      total: run.total,
      speedBps: run.paused ? 0 : run.speed,
      etaSecs: run.paused ? null : Math.max(0, Math.round((run.total - done) / Math.max(1, run.speed))),
      conns: run.conns.map(function (c) {
        return { index: c.index, done: c.done, size: c.size, active: c.active, phase: c.phase };
      })
    };
  }

  function tick(run) {
    if (run.paused || !run.timer) return;
    var per = (run.speed * 0.25) / run.conns.length;
    var reached = true;
    run.conns.forEach(function (c) {
      if (c.done < c.size) {
        c.done = Math.min(c.size, c.done + per);
        c.phase = "receiving";
        c.active = true;
      }
      if (c.done >= c.size) { c.phase = "done"; c.active = false; }
      else reached = false;
    });
    emit("hydra://progress", snapshot(run));
    // Never drift past the scripted fraction.
    var done = run.conns.reduce(function (a, c) { return a + c.done; }, 0);
    if (run.holdAt != null && done >= run.total * run.holdAt) {
      clearInterval(run.timer);
      run.timer = null;
    } else if (reached) {
      clearInterval(run.timer);
      run.timer = null;
      finish(run);
    }
  }

  function finish(run) {
    run.state = "done";
    emit("hydra://state", {
      id: run.id,
      state: "done",
      path: (run.destDir || settings.destDir) + "/" + run.filename,
      size: run.total
    });
  }

  function byName(fragment) {
    for (var id in runs) {
      if (runs[id].filename && runs[id].filename.indexOf(fragment) !== -1) return runs[id];
    }
    return null;
  }

  function pause(run) {
    run.paused = true;
    if (run.timer) { clearInterval(run.timer); run.timer = null; }
    emit("hydra://state", { id: run.id, state: "paused" });
  }

  function fail(run, message) {
    run.paused = true;
    if (run.timer) { clearInterval(run.timer); run.timer = null; }
    emit("hydra://state", { id: run.id, state: "error", message: message });
  }

  function probeFor(url) {
    var name = url.split("?")[0].split("/").pop() || "download.bin";
    return {
      url: url,
      size: 7772412965,
      acceptsRanges: true,
      filename: name,
      contentType: "application/octet-stream",
      probeMethod: "head",
      server: "nginx/1.25.3"
    };
  }

  function invoke(name, args) {
    args = args || {};
    return new Promise(function (resolve, reject) {
      var delay = SNAP ? 0 : (name === "probe_url" ? 380 : 30);
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
            resolve(probeFor(args.url));
            return;
          case "meta_check":
            resolve({ exists: false, compatible: false, bytesDone: 0 });
            return;
          case "pick_folder":
            settings.destDir = "/Users/you/Movies/Hydra";
            resolve(settings.destDir);
            return;
          case "start_download": {
            var id = "dl-" + nextId++;
            var scripted = queue.shift() || {};
            // Mirror the Rust limit (16), not an arbitrary 8.
            var n = Math.min(args.connections || 8, 16);
            var run = {
              id: id,
              filename: args.filename,
              destDir: args.destDir,
              total: args.size || 7772412965,
              connections: n,
              conns: plan(n, args.size || 7772412965),
              speed: 38170000,
              paused: false,
              state: "downloading",
              timer: null,
              holdAt: scripted.hold != null ? scripted.hold : null
            };
            runs[id] = run;
            resolve({ id: id });
            setTimeout(function () {
              // A cancel can land inside this window; a cancelled run must not
              // start emitting afterwards.
              if (!runs[id]) return;
              emit("hydra://state", { id: id, state: "downloading", note: null });
              // Land on the scripted fraction immediately so the demo is
              // deterministic; a fractional hold then keeps ticking so it looks
              // alive when watched rather than captured.
              if (run.holdAt != null) {
                run.conns.forEach(function (c) {
                  c.done = Math.min(c.size, c.size * run.holdAt);
                  c.phase = c.done >= c.size ? "done" : "receiving";
                  c.active = c.done < c.size;
                });
                emit("hydra://progress", snapshot(run));
                if (run.holdAt >= 1) {
                  finish(run);
                } else if (!SNAP) {
                  run.timer = setInterval(function () { tick(run); }, 250);
                }
              } else {
                run.timer = setInterval(function () { tick(run); }, SNAP ? 60 : 250);
              }
              if (scripted.after) scripted.after(run);
            }, SNAP ? 5 : 60);
            return;
          }
          case "pause_download": {
            var r1 = runs[args.id];
            if (r1) pause(r1);
            resolve(null);
            return;
          }
          case "resume_download": {
            var r2 = runs[args.id];
            if (r2) {
              r2.paused = false;
              r2.holdAt = null; // a resumed run keeps going past the scripted hold
              emit("hydra://state", { id: args.id, state: "downloading" });
              if (!r2.timer) r2.timer = setInterval(function () { tick(r2); }, SNAP ? 60 : 250);
            }
            resolve(null);
            return;
          }
          case "cancel_download":
            if (runs[args.id] && runs[args.id].timer) clearInterval(runs[args.id].timer);
            delete runs[args.id];
            resolve(null);
            return;
          case "reveal_path":
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
        var list = (listeners[name] = listeners[name] || []);
        list.push(cb);
        return Promise.resolve(function () {
          var at = list.indexOf(cb);
          if (at >= 0) list.splice(at, 1);
        });
      }
    }
  };

  // ---------- demo drivers ----------

  function wait(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }

  function frame() {
    return new Promise(function (r) {
      requestAnimationFrame(function () { requestAnimationFrame(r); });
    });
  }

  async function until(fn) {
    for (var i = 0; i < 400; i++) {
      if (fn()) return true;
      await wait(4);
    }
    return false;
  }

  function itemCount() { return document.querySelectorAll(".dl-item").length; }

  function fill(url) {
    var input = document.getElementById("url-input");
    input.value = url;
    input.dispatchEvent(new Event("input"));
  }

  function click(sel) { document.querySelector(sel).click(); }

  async function composeAndStart(url, filename, hold) {
    var before = itemCount();
    click("#new-btn");
    await frame();
    fill(url);
    click("#probe-btn");
    var ready = await until(function () {
      return !document.getElementById("primary-btn").disabled;
    });
    if (!ready) return false;
    if (filename) {
      var f = document.getElementById("filename-input");
      f.value = filename;
      f.dispatchEvent(new Event("input"));
      await frame();
    }
    queue.push({ hold: hold });
    click("#primary-btn");
    await until(function () { return itemCount() > before; });
    await frame();
    return true;
  }

  window.addEventListener("load", function () {
    if (!demo) return;
    // Hold one request open while the demo runs, so a headless capture waits for
    // the settled end state instead of racing the driver.
    var holdOpen = fetch("/__hold?demo").catch(function () {});
    setTimeout(async function () {
      if (demo === "compose") { fetch("/__release").catch(function () {}); return; }

      if (demo === "ready") {
        fill("https://origin.example/files/sample-dataset.tar.zst");
        click("#probe-btn");
        await until(function () { return !document.getElementById("primary-btn").disabled; });
        await frame();
        fetch("/__release").catch(function () {});
        return;
      }

      if (demo === "done") {
        await composeAndStart("https://origin.example/files/photos-archive.zip", null, 1.0);
        await frame();
        await holdOpen;
        fetch("/__release").catch(function () {});
        return;
      }

      if (demo === "error") {
        await composeAndStart("https://origin.example/files/old-build.tar", null, 0.12);
        await frame();
        var r = byName("old-build");
        if (r) fail(r, "connection lost mid-download (after 5 retries)");
        await frame();
        await holdOpen;
        fetch("/__release").catch(function () {});
        return;
      }

      if (demo === "multi") {
        await composeAndStart("https://origin.example/files/Backup-2026-11.tar.zst", null, 0.38);
        await composeAndStart("https://origin.example/files/film-2160p.mkv", null, 0.72);
        await composeAndStart("https://origin.example/files/disk-image.dmg", null, 0.24);
        await composeAndStart("https://origin.example/files/photos-archive.zip", null, 1.0);
        await composeAndStart("https://origin.example/files/old-build.tar", null, 0.12);

        await frame();
        var disk = byName("disk-image");
        if (disk) pause(disk);
        var broken = byName("old-build");
        if (broken) fail(broken, "connection lost mid-download (after 5 retries)");
        await frame();

        // land on a live transfer so the detail pane shows the comb working
        var names = document.querySelectorAll(".dl-row-name");
        for (var i = 0; i < names.length; i++) {
          if (names[i].textContent.indexOf("film-2160p") !== -1) { names[i].click(); break; }
        }
        await frame();
        await holdOpen;
        fetch("/__release").catch(function () {});
        return;
      }
    }, SNAP ? 40 : 260);
  });
})();
