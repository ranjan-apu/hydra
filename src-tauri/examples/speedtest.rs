//! Headless speedtest harness - CONTRACT "Headless speedtest harness":
//!
//!   speedtest <url> [conns=4] [max_bytes=1073741824] [out=/tmp/hydra-speed.part]
//!             [run_seconds=0] [cancel_after_secs=0]
//!
//! Probes, then downloads the FIRST max_bytes bytes of <url> with conns static
//! chunks over [0, max_bytes) using THE SAME engine core as the app (no
//! copy-pasted logic). run_seconds > 0 pauses after that many seconds of
//! ACTIVE downloading exactly like the app (sidecar kept for resume).
//! cancel_after_secs > 0 cancels instead, exactly like the app: the partial
//! .part and its .hydra.json sidecar must be gone afterwards.
//! Prints one TICK line per second and a machine-readable SUMMARY line; exit
//! code 0 iff ok=1.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use hydra_lib::engine::download::{Ctrl, DownloadSpec};
use hydra_lib::engine::types::EngineEvent;
use hydra_lib::engine::{http_client, meta, probe, run};

const DEFAULT_MAX_BYTES: u64 = 1073741824;
const DEFAULT_OUT: &str = "/tmp/hydra-speed.part";

#[tokio::main]
async fn main() {
    std::process::exit(real_main().await);
}

async fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "usage: speedtest <url> [conns=4] [max_bytes=1073741824] [out=/tmp/hydra-speed.part] [run_seconds=0] [cancel_after_secs=0]"
        );
        println!("SUMMARY ok=0 error=missing url");
        return 1;
    }
    // "--probe <url>": report what the probe decided and exit, without pulling a
    // single byte of body. Acceptance pins the range-verification and
    // unknown-size behaviours through this.
    if args[0] == "--probe" {
        let Some(target) = args.get(1) else {
            println!("SUMMARY ok=0 error=missing url");
            return 1;
        };
        let client = http_client();
        return match probe::probe(target, &client).await {
            Ok(i) => {
                println!(
                    "PROBE acceptsRanges={} size={} method={} server={}",
                    i.accepts_ranges,
                    i.size
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "null".into()),
                    i.probe_method,
                    i.server.unwrap_or_else(|| "null".into())
                );
                0
            }
            Err(e) => {
                println!("SUMMARY ok=0 error={}", e);
                1
            }
        };
    }
    let url = args[0].clone();
    let conns: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    let max_bytes: u64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_BYTES);
    let out: String = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| DEFAULT_OUT.to_string());
    let run_seconds: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let cancel_after: u64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
    let conns = conns.clamp(1, 16) as usize;

    // ---- probe (same core as the app) ----
    let client = http_client();
    let info = match probe::probe(&url, &client).await {
        Ok(i) => i,
        Err(e) => {
            println!("SUMMARY ok=0 error={}", e);
            return 1;
        }
    };
    // Unknown total (CONTRACT size = null): single sequential stream from 0, no
    // sidecar and no resume - the engine stops when the server closes. Otherwise
    // download the FIRST max_bytes bytes (or the whole file when smaller).
    let eff = info.size.map(|s| s.min(max_bytes));

    let base = out.strip_suffix(".part").unwrap_or(&out).to_string();
    let part_path = PathBuf::from(&out);
    let meta_path = PathBuf::from(format!("{}.hydra.json", base));
    let final_path = PathBuf::from(&base);

    // ---- resume detection (the same engine helpers the engine itself uses) ----
    let mut resumed: u8 = 0;
    let mut start_bytes: u64 = 0;
    let single_meta = !info.accepts_ranges;
    if part_path.exists() {
        if let Some(m) = meta::load(&meta_path) {
            let compatible = eff.is_some_and(|e| meta::is_compatible(&m, &info.url, e));
            if compatible && !(single_meta && m.chunks.len() > 1) {
                let part_len = std::fs::metadata(&part_path).map(|s| s.len()).unwrap_or(0);
                let mut m = m;
                meta::clamp_dones(&mut m, part_len);
                let done = meta::bytes_done(&m);
                if done > 0 {
                    resumed = 1;
                    start_bytes = done;
                }
            }
        }
    }
    if resumed == 1 {
        println!("RESUMED bytes={}", start_bytes);
    }

    // ---- run the engine core ----
    let spec = DownloadSpec {
        url: info.url.clone(),
        part_path,
        meta_path,
        final_path,
        connections: conns,
        // Virtual size = the first max_bytes bytes; the Content-Range
        // cross-check still validates against the real probed total.
        size: eff,
        probed_total: info.size,
        accepts_ranges: info.accepts_ranges,
    };
    // Honest wall-clock average of the transfer: probe excluded.
    let transfer_start = Instant::now();
    // Same bounded queue the app uses, so the harness exercises the real
    // delivery policy rather than an unbounded stand-in.
    let (evt_tx, mut evt_rx) =
        tokio::sync::mpsc::channel::<EngineEvent>(hydra_lib::engine::download::EVENT_QUEUE_CAP);
    let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::unbounded_channel::<Ctrl>();
    tokio::spawn(run(spec, evt_tx, ctrl_rx));

    let mut single: u8 = if info.accepts_ranges { 0 } else { 1 };
    let mut done_now: u64 = start_bytes;
    let mut last_tick: u64 = 0;
    let mut pause_armed = false;
    let mut pause_sleep = Box::pin(tokio::time::sleep(Duration::from_secs(86400 * 365)));
    let mut cancel_armed = false;
    let mut cancelled = false;
    let mut cancel_sleep = Box::pin(tokio::time::sleep(Duration::from_secs(86400 * 365)));

    loop {
        tokio::select! {
            ev = evt_rx.recv() => match ev {
                None => {
                    if cancelled {
                        // The engine has returned, which means its cancel
                        // cleanup already ran: report, and let the caller
                        // assert the filesystem really is clean.
                        let elapsed = transfer_start.elapsed().as_secs_f64();
                        let this_run = done_now.saturating_sub(start_bytes);
                        let avg = if elapsed > 0.0 { this_run as f64 / elapsed / 1048576.0 } else { 0.0 };
                        println!("CANCELLED bytes={}", done_now);
                        println!(
                            "SUMMARY ok=1 bytes={} elapsed_s={:.1} avg_mibps={:.1} resumed={} single={} cancelled=1",
                            done_now, elapsed, avg, resumed, single
                        );
                        return 0;
                    }
                    println!("SUMMARY ok=0 error=engine stopped unexpectedly");
                    return 1;
                }
                Some(EngineEvent::State(s)) => {
                    if s.state == "downloading" {
                        if run_seconds > 0 && !pause_armed {
                            pause_armed = true;
                            pause_sleep
                                .as_mut()
                                .reset(tokio::time::Instant::now() + Duration::from_secs(run_seconds));
                        }
                        if cancel_after > 0 && !cancel_armed {
                            cancel_armed = true;
                            cancel_sleep
                                .as_mut()
                                .reset(tokio::time::Instant::now() + Duration::from_secs(cancel_after));
                        }
                        if let Some(n) = &s.note {
                            if n.contains("single connection") {
                                single = 1;
                            }
                        }
                    } else if s.state == "paused" {
                        // The engine flushes the sidecar before this event.
                        println!("PAUSED bytes={}", done_now);
                        return 0;
                    } else if s.state == "done" {
                        let bytes = s.size.unwrap_or(done_now);
                        let elapsed = transfer_start.elapsed().as_secs_f64();
                        let this_run = bytes.saturating_sub(start_bytes);
                        let avg = if elapsed > 0.0 {
                            this_run as f64 / elapsed / 1048576.0
                        } else {
                            0.0
                        };
                        println!(
                            "SUMMARY ok=1 bytes={} elapsed_s={:.1} avg_mibps={:.1} resumed={} single={}",
                            bytes,
                            elapsed,
                            avg,
                            resumed,
                            single
                        );
                        return 0;
                    } else if s.state == "error" {
                        println!(
                            "SUMMARY ok=0 error={}",
                            s.message.unwrap_or_else(|| "unknown error".into())
                        );
                        return 1;
                    }
                }
                Some(EngineEvent::Progress(p)) => {
                    done_now = p.bytes_done;
                    let whole = transfer_start.elapsed().as_secs();
                    if whole > last_tick {
                        last_tick = whole;
                        println!(
                            "TICK t={} done={} speed_mibps={:.1}",
                            whole,
                            p.bytes_done,
                            p.speed_bps as f64 / 1048576.0
                        );
                    }
                }
            },
            _ = &mut pause_sleep, if pause_armed => {
                pause_armed = false;
                if !cancelled {
                    let _ = ctrl_tx.send(Ctrl::Pause);
                }
            },
            _ = &mut cancel_sleep, if cancel_armed => {
                cancel_armed = false;
                cancelled = true;
                let _ = ctrl_tx.send(Ctrl::Cancel(None));
            },
        }
    }
}
