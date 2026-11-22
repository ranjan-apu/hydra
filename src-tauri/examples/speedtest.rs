//! Headless speedtest harness - CONTRACT "Headless speedtest harness":
//!
//!   speedtest <url> [conns=4] [max_bytes=1073741824] [out=/tmp/hydra-speed.part] [run_seconds=0]
//!
//! Probes, then downloads the FIRST max_bytes bytes of <url> with conns static
//! chunks over [0, max_bytes) using THE SAME engine core as the app (no
//! copy-pasted logic). run_seconds > 0 pauses after that many seconds of
//! ACTIVE downloading exactly like the app (sidecar kept for resume).
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
            "usage: speedtest <url> [conns=4] [max_bytes=1073741824] [out=/tmp/hydra-speed.part] [run_seconds=0]"
        );
        println!("SUMMARY ok=0 error=missing url");
        return 1;
    }
    let url = args[0].clone();
    let conns: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    let max_bytes: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_MAX_BYTES);
    let out: String = args.get(3).cloned().unwrap_or_else(|| DEFAULT_OUT.to_string());
    let run_seconds: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
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
    let full_size = match info.size {
        Some(s) => s,
        None => {
            println!("SUMMARY ok=0 error=probe returned unknown size");
            return 1;
        }
    };
    // Download the FIRST max_bytes bytes (or the whole file when smaller).
    let eff = full_size.min(max_bytes);

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
            if meta::is_compatible(&m, &info.url, eff) && !(single_meta && m.chunks.len() > 1) {
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
        size: Some(eff),
        probed_total: Some(full_size),
        accepts_ranges: info.accepts_ranges,
    };
    // Honest wall-clock average of the transfer: probe excluded.
    let transfer_start = Instant::now();
    let (evt_tx, mut evt_rx) = tokio::sync::mpsc::unbounded_channel::<EngineEvent>();
    let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::unbounded_channel::<Ctrl>();
    tokio::spawn(run(spec, evt_tx, ctrl_rx));

    let mut single: u8 = if info.accepts_ranges { 0 } else { 1 };
    let mut done_now: u64 = start_bytes;
    let mut last_tick: u64 = 0;
    let mut pause_armed = false;
    let mut pause_sleep = Box::pin(tokio::time::sleep(Duration::from_secs(86400 * 365)));

    loop {
        tokio::select! {
            ev = evt_rx.recv() => match ev {
                None => {
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
                let _ = ctrl_tx.send(Ctrl::Pause);
            },
        }
    }
}
