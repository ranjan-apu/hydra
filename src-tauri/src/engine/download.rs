//! The download engine core: plan setup (sidecar resume or fresh), ranged and
//! single-stream workers, the progress monitor (250 ms ticks, ~2 s rolling
//! speed, <= 1/s meta flushes), pause/resume/cancel supervision through a
//! CancellationToken, the graceful single-stream fallback, and completion
//! (fsync -> verify -> rename -> delete meta).
//!
//! Tauri-free: the engine reports through an mpsc channel of EngineEvent and
//! is shared verbatim by the app bridge and the headless speedtest harness.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::meta::{self, MetaChunk, MetaFile};
use super::plan::{self, ChunkSpec};
use super::types::{
    ConnInfo, EngineEvent, ProgressPayload, StatePayload, PHASE_CONNECTING, PHASE_DONE, PHASE_IDLE,
    PHASE_RECEIVING, PHASE_RETRYING, phase_str,
};

/// Streaming block size: each pwrite covers at most ~128 KiB, and the
/// cancellation token is checked between blocks.
pub const BLOCK_SIZE: usize = 128 * 1024;
/// Attempts per chunk (5 total: initial + 4 retries), per CONTRACT.
const MAX_ATTEMPTS: u32 = 5;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const SPEED_WINDOW: Duration = Duration::from_secs(2);
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Bounded wait for worker shutdown on pause (~5 s, then abort).
const JOIN_BUDGET: Duration = Duration::from_secs(5);
const ERROR_JOIN_BUDGET: Duration = Duration::from_secs(2);
/// Retry backoff: 500ms * 2^n capped at 8 s.
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CAP: Duration = Duration::from_secs(8);

/// Control messages the embedder sends into a running engine.
#[derive(Debug, Clone, Copy)]
pub enum Ctrl {
    Pause,
    Resume,
    Cancel,
}

/// A download to run. Paths are explicit so the app bridge and the speedtest
/// harness drive the exact same engine.
#[derive(Debug, Clone)]
pub struct DownloadSpec {
    /// FINAL resolved URL from the probe; workers do not re-follow redirects.
    pub url: String,
    pub part_path: PathBuf,
    pub meta_path: PathBuf,
    /// Rename target on completion ("<stem> (k)<ext>" on collision).
    pub final_path: PathBuf,
    /// 1..=16; defensively clamped to >= 1 here.
    pub connections: usize,
    /// Download target size: how many bytes we intend to place on disk. The
    /// app sets this to the probed size; the speedtest sets it to
    /// min(probed size, max_bytes) and downloads the first max_bytes bytes.
    /// None = unknown total (single-stream to EOF, no sidecar).
    pub size: Option<u64>,
    /// The server-reported total from the probe, used ONLY to cross-check the
    /// Content-Range "bytes S-E/T" total. In the app this equals size; the
    /// speedtest passes the real probed total while size is the virtual
    /// capped size, so a truthful CDN still validates.
    pub probed_total: Option<u64>,
    pub accepts_ranges: bool,
}

struct PlanChunk {
    start: u64,
    /// None = stream to EOF (unknown total).
    len: Option<u64>,
    /// Seed for the shared counter (resume offsets from the sidecar).
    done: u64,
}

struct Plan {
    size: Option<u64>,
    chunk_size: u64,
    chunks: Vec<PlanChunk>,
    /// true = one sequential connection, 206 not required.
    single: bool,
}

impl Plan {
    /// CONTRACT fresh ranged plan: chunkSize = ceil(size / connections).
    fn ranged(size: u64, connections: usize) -> Plan {
        let cs = plan::chunk_size(size, connections);
        let chunks = plan::build_chunks(size, connections)
            .into_iter()
            .map(|c: ChunkSpec| PlanChunk { start: c.start, len: Some(c.len()), done: 0 })
            .collect();
        Plan { size: Some(size), chunk_size: cs, chunks, single: false }
    }

    /// One sequential connection from 0.
    fn single(size: Option<u64>) -> Plan {
        Plan {
            size,
            chunk_size: size.unwrap_or(0),
            chunks: vec![PlanChunk { start: 0, len: size, done: 0 }],
            single: true,
        }
    }

    /// Adopt a validated sidecar; dones were clamped by the caller.
    fn from_meta(m: &MetaFile, single: bool) -> Plan {
        let chunks = m
            .chunks
            .iter()
            .map(|c| PlanChunk {
                start: c.start,
                len: Some(c.end - c.start + 1),
                done: c.done,
            })
            .collect();
        Plan { size: Some(m.size), chunk_size: m.chunk_size, chunks, single }
    }
}

struct WorkerShared {
    start: u64,
    len: Option<u64>,
    done: AtomicU64,
    phase: AtomicU8,
    active: AtomicBool,
}

struct RunShared {
    workers: Vec<Arc<WorkerShared>>,
    /// True once any worker has flushed a block to disk; decides the
    /// "refused ranges" fallback vs. mid-download failure.
    any_written: Arc<AtomicBool>,
    speed_bps: AtomicU64,
}

enum FatalKind {
    Http(u16),
    RangeMismatch,
    RetryExhausted,
    RefusedRangesStart,
    RefusedRangesMid,
    EmptyResponse,
    Io(String),
}

impl FatalKind {
    fn message(&self) -> String {
        match self {
            FatalKind::Http(c) => format!("HTTP {}", c),
            FatalKind::RangeMismatch => "server returned an unexpected Content-Range".into(),
            FatalKind::RetryExhausted => "connection lost mid-download (after 5 retries)".into(),
            FatalKind::RefusedRangesStart => {
                "server refused range requests - using a single connection".into()
            }
            FatalKind::RefusedRangesMid => "server refused range requests mid-download".into(),
            FatalKind::EmptyResponse => "empty response from server".into(),
            FatalKind::Io(m) => m.clone(),
        }
    }
}

enum WorkerMsg {
    Complete,
    Fatal(FatalKind),
}

struct WorkerCtx {
    client: reqwest::Client,
    url: String,
    part_path: PathBuf,
    /// Server-reported total from the probe, for Content-Range cross-checks.
    total_ref: Option<u64>,
    single: bool,
    token: CancellationToken,
    shared: Arc<WorkerShared>,
    any_written: Arc<AtomicBool>,
    msgs: UnboundedSender<WorkerMsg>,
}

fn open_part(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create(true).truncate(false).open(path)
}

fn map_io_error(e: &io::Error, path: &Path) -> String {
    if e.raw_os_error() == Some(28) || e.kind() == io::ErrorKind::StorageFull {
        return "disk full".to_string();
    }
    match e.kind() {
        io::ErrorKind::PermissionDenied
        | io::ErrorKind::NotFound
        | io::ErrorKind::NotADirectory
        | io::ErrorKind::IsADirectory => format!("destination is not writable: {}", path.display()),
        _ => format!("io error: {}", e),
    }
}

fn emit_state(events: &UnboundedSender<EngineEvent>, payload: StatePayload) {
    let _ = events.send(EngineEvent::State(payload));
}

fn state_payload(state: &str) -> StatePayload {
    StatePayload {
        state: state.to_string(),
        path: None,
        size: None,
        message: None,
        note: None,
    }
}

/// Positional write of one accumulated block; updates the shared done counter
/// only after the bytes are on disk (the meta never claims unwritten data).
fn write_block(
    file: &mut File,
    buf: &mut Vec<u8>,
    shared: &WorkerShared,
    done: u64,
    any_written: &AtomicBool,
    part_path: &Path,
) -> Result<u64, String> {
    if buf.is_empty() {
        return Ok(done);
    }
    let offset = shared.start + done;
    if let Err(e) = file.write_all_at(buf, offset) {
        return Err(map_io_error(&e, part_path));
    }
    let new_done = done + buf.len() as u64;
    shared.done.store(new_done, Ordering::SeqCst);
    any_written.store(true, Ordering::SeqCst);
    buf.clear();
    Ok(new_done)
}

fn fatal(ctx: &WorkerCtx, kind: FatalKind) {
    let _ = ctx.msgs.send(WorkerMsg::Fatal(kind));
}

/// Cap the pending buffer at the remaining bytes of a known target so the
/// single-stream worker never writes past the end of the intended range (a
/// 200 full-body response can be larger than the target, e.g. a speedtest
/// prefix of a bigger file).
fn trim_to_target(buf: &mut Vec<u8>, target: Option<u64>, done: u64) {
    if let Some(t) = target {
        let remaining = t.saturating_sub(done) as usize;
        if buf.len() > remaining {
            buf.truncate(remaining);
        }
    }
}

fn complete(ctx: &WorkerCtx) {
    ctx.shared.phase.store(PHASE_DONE, Ordering::SeqCst);
    let _ = ctx.msgs.send(WorkerMsg::Complete);
}

/// Backoff before the next attempt: 500ms * 2^n capped at 8 s, under the
/// cancellation token. Returns false when the token fired.
async fn backoff(ctx: &WorkerCtx, failed_attempt: u32) -> bool {
    ctx.shared.phase.store(PHASE_RETRYING, Ordering::SeqCst);
    let shift = (failed_attempt - 1).min(4);
    let mut d = Duration::from_millis(BACKOFF_BASE_MS.saturating_mul(1u64 << shift));
    if d > BACKOFF_CAP {
        d = BACKOFF_CAP;
    }
    tokio::select! {
        biased;
        _ = ctx.token.cancelled() => false,
        _ = tokio::time::sleep(d) => true,
    }
}

async fn run_worker(ctx: WorkerCtx) {
    ctx.shared.active.store(true, Ordering::SeqCst);
    let result = if ctx.single { run_single(&ctx).await } else { run_ranged(&ctx).await };
    ctx.shared.active.store(false, Ordering::SeqCst);
    if ctx.token.is_cancelled() {
        // Stopped by pause/cancel, not by finishing the chunk.
        ctx.shared.phase.store(PHASE_IDLE, Ordering::SeqCst);
    }
    result
}

/// Ranged worker: chunk i for the whole run; REQUIRE 206 with a matching
/// Content-Range; retries the same chunk from its current offset.
async fn run_ranged(ctx: &WorkerCtx) {
    let len = match ctx.shared.len {
        Some(l) => l,
        None => return,
    };
    let mut file = match open_part(&ctx.part_path) {
        Ok(f) => f,
        Err(e) => {
            fatal(ctx, FatalKind::Io(map_io_error(&e, &ctx.part_path)));
            return;
        }
    };
    let mut attempt: u32 = 0;
    'attempts: loop {
        let mut done = ctx.shared.done.load(Ordering::SeqCst);
        if done >= len {
            complete(ctx);
            return;
        }
        attempt += 1;
        if attempt > MAX_ATTEMPTS {
            fatal(ctx, FatalKind::RetryExhausted);
            return;
        }
        ctx.shared.phase.store(PHASE_CONNECTING, Ordering::SeqCst);
        let req_start = ctx.shared.start + done;
        let req_end = ctx.shared.start + len - 1;
        let resp = {
            let req = ctx
                .client
                .get(&ctx.url)
                .header(reqwest::header::RANGE, format!("bytes={}-{}", req_start, req_end));
            tokio::select! {
                biased;
                _ = ctx.token.cancelled() => return,
                r = req.send() => r
            }
        };
        let resp = match resp {
            Ok(r) => r,
            Err(_) => {
                if !backoff(ctx, attempt).await {
                    return;
                }
                continue 'attempts;
            }
        };
        let status = resp.status().as_u16();
        if status == 200 {
            // The server ignored our Range. Before any byte was written this
            // triggers the graceful single-stream fallback; afterwards it is a
            // hard error (the supervisor decides).
            if ctx.any_written.load(Ordering::SeqCst) {
                fatal(ctx, FatalKind::RefusedRangesMid);
            } else {
                fatal(ctx, FatalKind::RefusedRangesStart);
            }
            return;
        }
        if status == 206 {
            let cr = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(plan::parse_content_range);
            let ok = match cr {
                None => false,
                Some(cr) => {
                    let start_ok = cr.start == req_start;
                    let end_ok = cr.end.map_or(true, |e| e == req_end);
                    let total_ok = match (cr.total, ctx.total_ref) {
                        (Some(t), Some(s)) => t == s,
                        _ => true,
                    };
                    start_ok && end_ok && total_ok
                }
            };
            if !ok {
                fatal(ctx, FatalKind::RangeMismatch);
                return;
            }
        } else {
            let code = status;
            if code == 429 || (500..600).contains(&code) {
                if !backoff(ctx, attempt).await {
                    return;
                }
                continue 'attempts;
            }
            fatal(ctx, FatalKind::Http(code));
            return;
        }
        // Stream the body in ~128 KiB blocks with positional writes.
        ctx.shared.phase.store(PHASE_RECEIVING, Ordering::SeqCst);
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(BLOCK_SIZE);
        loop {
            if buf.len() >= BLOCK_SIZE {
                match write_block(&mut file, &mut buf, &ctx.shared, done, &ctx.any_written, &ctx.part_path) {
                    Ok(d) => {
                        done = d;
                        if done >= len {
                            complete(ctx);
                            return;
                        }
                    }
                    Err(m) => {
                        fatal(ctx, FatalKind::Io(m));
                        return;
                    }
                }
            }
            tokio::select! {
                biased;
                _ = ctx.token.cancelled() => return,
                item = stream.next() => match item {
                    None => {
                        match write_block(&mut file, &mut buf, &ctx.shared, done, &ctx.any_written, &ctx.part_path) {
                            Ok(d) => done = d,
                            Err(m) => {
                                fatal(ctx, FatalKind::Io(m));
                                return;
                            }
                        }
                        if done >= len {
                            complete(ctx);
                            return;
                        }
                        // Early EOF: the server closed before the chunk end.
                        break;
                    }
                    Some(Err(_)) => {
                        // Mid-body error: retry from the current offset.
                        break;
                    }
                    Some(Ok(chunk)) => {
                        buf.extend_from_slice(&chunk);
                    }
                },
            }
        }
        if !backoff(ctx, attempt).await {
            return;
        }
        continue 'attempts;
    }
}

/// Single-stream worker: one sequential connection from the current offset.
/// On resume it tries "Range: bytes=done-"; a server that ignores the header
/// answers 200 and the first done bytes are read and discarded. A completely
/// empty response at offset 0 is "empty response from server".
async fn run_single(ctx: &WorkerCtx) {
    let target = ctx.shared.len;
    let mut file = match open_part(&ctx.part_path) {
        Ok(f) => f,
        Err(e) => {
            fatal(ctx, FatalKind::Io(map_io_error(&e, &ctx.part_path)));
            return;
        }
    };
    let mut attempt: u32 = 0;
    'attempts: loop {
        let mut done = ctx.shared.done.load(Ordering::SeqCst);
        if let Some(t) = target {
            if done >= t {
                complete(ctx);
                return;
            }
        }
        attempt += 1;
        if attempt > MAX_ATTEMPTS {
            fatal(ctx, FatalKind::RetryExhausted);
            return;
        }
        ctx.shared.phase.store(PHASE_CONNECTING, Ordering::SeqCst);
        let mut req = ctx.client.get(&ctx.url);
        if done > 0 {
            let range = match target {
                Some(t) => format!("bytes={}-{}", done, t - 1),
                None => format!("bytes={}-", done),
            };
            req = req.header(reqwest::header::RANGE, range);
        }
        let resp = {
            tokio::select! {
                biased;
                _ = ctx.token.cancelled() => return,
                r = req.send() => r
            }
        };
        let resp = match resp {
            Ok(r) => r,
            Err(_) => {
                if !backoff(ctx, attempt).await {
                    return;
                }
                continue 'attempts;
            }
        };
        let status = resp.status().as_u16();
        let mut skip: u64 = 0;
        if status == 206 {
            let cr = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(plan::parse_content_range);
            let ok = match cr {
                None => false,
                Some(cr) => cr.start == done,
            };
            if !ok {
                fatal(ctx, FatalKind::RangeMismatch);
                return;
            }
        } else if status == 200 {
            if done > 0 {
                // Body starts at 0: discard what is already on disk.
                skip = done;
            }
        } else {
            let code = status;
            if code == 429 || (500..600).contains(&code) {
                if !backoff(ctx, attempt).await {
                    return;
                }
                continue 'attempts;
            }
            fatal(ctx, FatalKind::Http(code));
            return;
        }
        ctx.shared.phase.store(PHASE_RECEIVING, Ordering::SeqCst);
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(BLOCK_SIZE);
        let mut received: u64 = 0;
        loop {
            if buf.len() >= BLOCK_SIZE {
                trim_to_target(&mut buf, target, done);
                match write_block(&mut file, &mut buf, &ctx.shared, done, &ctx.any_written, &ctx.part_path) {
                    Ok(d) => {
                        done = d;
                        if let Some(t) = target {
                            if done >= t {
                                complete(ctx);
                                return;
                            }
                        }
                    }
                    Err(m) => {
                        fatal(ctx, FatalKind::Io(m));
                        return;
                    }
                }
            }
            tokio::select! {
                biased;
                _ = ctx.token.cancelled() => return,
                item = stream.next() => match item {
                    None => {
                        trim_to_target(&mut buf, target, done);
                        match write_block(&mut file, &mut buf, &ctx.shared, done, &ctx.any_written, &ctx.part_path) {
                            Ok(d) => done = d,
                            Err(m) => {
                                fatal(ctx, FatalKind::Io(m));
                                return;
                            }
                        }
                        if received == 0 && done == 0 && skip == 0 {
                            fatal(ctx, FatalKind::EmptyResponse);
                            return;
                        }
                        if let Some(t) = target {
                            if done >= t {
                                complete(ctx);
                                return;
                            }
                        } else {
                            // Unknown total: EOF is the end of the stream.
                            complete(ctx);
                            return;
                        }
                        // Early EOF with a known target: retryable.
                        break;
                    }
                    Some(Err(_)) => {
                        break;
                    }
                    Some(Ok(chunk)) => {
                        received += chunk.len() as u64;
                        if skip > 0 {
                            let take = (skip.min(chunk.len() as u64)) as usize;
                            skip -= take as u64;
                            if take == chunk.len() {
                                continue;
                            }
                            buf.extend_from_slice(&chunk[take..]);
                        } else {
                            buf.extend_from_slice(&chunk);
                        }
                    }
                },
            }
        }
        if !backoff(ctx, attempt).await {
            return;
        }
        continue 'attempts;
    }
}

/// One progress entry; also the final progress before paused/done.
fn build_progress(run: &RunShared, total: Option<u64>, speed_bps: u64) -> ProgressPayload {
    let bytes_done: u64 = run.workers.iter().map(|w| w.done.load(Ordering::SeqCst)).sum();
    let eta_secs = match total {
        Some(t) if speed_bps > 0 => {
            Some(((t.saturating_sub(bytes_done)) as f64 / speed_bps as f64).round() as u64)
        }
        _ => None,
    };
    let conns = run
        .workers
        .iter()
        .enumerate()
        .map(|(i, w)| ConnInfo {
            index: i,
            done: w.done.load(Ordering::SeqCst),
            size: w.len,
            active: w.active.load(Ordering::SeqCst),
            phase: phase_str(w.phase.load(Ordering::SeqCst)).to_string(),
        })
        .collect();
    ProgressPayload { bytes_done, total, speed_bps, eta_secs, conns }
}

struct MetaFlushState {
    path: PathBuf,
    url: String,
    size: u64,
    chunk_size: u64,
    last_dones: Vec<u64>,
    last_flush: Instant,
}

struct MonitorCtx {
    run: Arc<RunShared>,
    events: UnboundedSender<EngineEvent>,
    token: CancellationToken,
    total: Option<u64>,
    /// None when the total is unknown (no sidecar for this download).
    meta: Option<MetaFlushState>,
}

/// Aggregate worker counters and serialize the current chunk state.
fn save_meta_snapshot(path: &Path, url: &str, size: u64, chunk_size: u64, workers: &[Arc<WorkerShared>]) -> io::Result<()> {
    let chunks: Vec<MetaChunk> = workers
        .iter()
        .map(|w| MetaChunk {
            start: w.start,
            end: w.start + w.len.unwrap_or(0).saturating_sub(1),
            done: w.done.load(Ordering::SeqCst),
        })
        .collect();
    let m = MetaFile { url: url.to_string(), size, chunk_size, chunks };
    meta::save_atomic(path, &m)
}

/// 250 ms progress monitor + <= 1/s dirty meta flusher.
async fn monitor_task(mut ctx: MonitorCtx) {
    let mut window: VecDeque<(Instant, u64)> = VecDeque::new();
    let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
    loop {
        tokio::select! {
            biased;
            _ = ctx.token.cancelled() => break,
            _ = ticker.tick() => {
                let now = Instant::now();
                let dones: Vec<u64> = ctx.run.workers.iter().map(|w| w.done.load(Ordering::SeqCst)).collect();
                let bytes_done: u64 = dones.iter().sum();
                window.push_back((now, bytes_done));
                while window.len() > 2 {
                    match window.front() {
                        Some((t, _)) if now.duration_since(*t) > SPEED_WINDOW + PROGRESS_INTERVAL => {
                            window.pop_front();
                        }
                        _ => break,
                    }
                }
                let mut speed_bps: u64 = 0;
                if let (Some((t0, d0)), Some((t1, d1))) = (window.front().copied(), window.back().copied()) {
                    let dt = t1.duration_since(t0).as_secs_f64();
                    if dt > 0.0 {
                        let dv = d1.saturating_sub(d0) as f64;
                        speed_bps = (dv / dt) as u64;
                    }
                }
                ctx.run.speed_bps.store(speed_bps, Ordering::SeqCst);
                let _ = ctx.events.send(EngineEvent::Progress(build_progress(&ctx.run, ctx.total, speed_bps)));
                if let Some(m) = ctx.meta.as_mut() {
                    if m.last_dones != dones {
                        if now.duration_since(m.last_flush) >= FLUSH_INTERVAL {
                            if save_meta_snapshot(&m.path, &m.url, m.size, m.chunk_size, &ctx.run.workers).is_ok() {
                                m.last_dones = dones;
                                m.last_flush = now;
                            }
                        }
                    }
                }
            }
        }
    }
}

struct Active {
    token: CancellationToken,
    handles: Vec<JoinHandle<()>>,
    msgs_rx: UnboundedReceiver<WorkerMsg>,
    completed: usize,
}

fn build_run(plan: &Plan) -> RunShared {
    let workers = plan
        .chunks
        .iter()
        .map(|c| {
            Arc::new(WorkerShared {
                start: c.start,
                len: c.len,
                done: AtomicU64::new(c.done),
                phase: AtomicU8::new(PHASE_IDLE),
                active: AtomicBool::new(false),
            })
        })
        .collect();
    RunShared {
        workers,
        any_written: Arc::new(AtomicBool::new(false)),
        speed_bps: AtomicU64::new(0),
    }
}

fn spawn_all(
    spec: &DownloadSpec,
    plan: &Plan,
    run: &Arc<RunShared>,
    events: &UnboundedSender<EngineEvent>,
    token: &CancellationToken,
) -> Active {
    let (msgs_tx, msgs_rx) = unbounded_channel();
    let client = super::http_client();
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    for w in run.workers.iter() {
        let ctx = WorkerCtx {
            client: client.clone(),
            url: spec.url.clone(),
            part_path: spec.part_path.clone(),
            total_ref: spec.probed_total,
            single: plan.single,
            token: token.clone(),
            shared: Arc::clone(w),
            any_written: Arc::clone(&run.any_written),
            msgs: msgs_tx.clone(),
        };
        handles.push(tokio::spawn(run_worker(ctx)));
    }
    let monitor_meta = plan.size.map(|size| MetaFlushState {
        path: spec.meta_path.clone(),
        url: spec.url.clone(),
        size,
        chunk_size: plan.chunk_size,
        last_dones: run.workers.iter().map(|w| w.done.load(Ordering::SeqCst)).collect(),
        last_flush: Instant::now(),
    });
    handles.push(tokio::spawn(monitor_task(MonitorCtx {
        run: Arc::clone(run),
        events: events.clone(),
        token: token.clone(),
        total: plan.size,
        meta: monitor_meta,
    })));
    Active { token: token.clone(), handles, msgs_rx, completed: 0 }
}

/// Await every handle within the remaining budget, then abort stragglers.
async fn join_bounded(handles: &mut Vec<JoinHandle<()>>, budget: Duration) {
    let deadline = Instant::now() + budget;
    for h in handles.iter_mut() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let _ = timeout(remaining, Pin::new(&mut *h)).await;
    }
    for h in handles.iter_mut() {
        h.abort();
    }
}

/// Flush the sidecar from the current worker counters (known size only).
fn flush_meta(spec: &DownloadSpec, plan: &Plan, run: &RunShared) {
    if let Some(size) = plan.size {
        let _ = save_meta_snapshot(&spec.meta_path, &spec.url, size, plan.chunk_size, &run.workers);
    }
}

/// Cancel_download cleanup: stop was requested already; delete the .part and
/// the sidecar. No state event afterwards (CONTRACT).
fn cleanup_cancel(spec: &DownloadSpec) {
    let _ = fs::remove_file(&spec.part_path);
    let _ = fs::remove_file(&spec.meta_path);
}

/// Sidecar resume when possible, otherwise a fresh plan.
fn setup_plan(spec: &DownloadSpec, note: &mut Option<String>) -> Plan {
    if let Some(size) = spec.size {
        let single = !spec.accepts_ranges;
        if spec.part_path.exists() {
            if let Some(m) = meta::load(&spec.meta_path) {
                if meta::is_compatible(&m, &spec.url, size) && !(single && m.chunks.len() > 1) {
                    let part_len = fs::metadata(&spec.part_path).map(|s| s.len()).unwrap_or(0);
                    let mut m = m;
                    meta::clamp_dones(&mut m, part_len);
                    *note = Some("Resuming partial download".to_string());
                    return Plan::from_meta(&m, single);
                }
            }
        }
        if spec.accepts_ranges {
            Plan::ranged(size, spec.connections.max(1))
        } else {
            Plan::single(Some(size))
        }
    } else {
        Plan::single(None)
    }
}

/// Completion: stop the monitor, emit final totals, fsync, verify the size,
/// rename (collision -> "<stem> (k)<ext>"), delete the meta, emit done.
async fn finalize(
    spec: &DownloadSpec,
    plan: &Plan,
    run: &RunShared,
    events: &UnboundedSender<EngineEvent>,
    active: &mut Active,
    coord: &File,
) {
    active.token.cancel();
    join_bounded(&mut active.handles, Duration::from_secs(1)).await;
    let speed = run.speed_bps.load(Ordering::SeqCst);
    let _ = events.send(EngineEvent::Progress(build_progress(run, plan.size, speed)));

    if let Err(e) = coord.sync_all() {
        let mut s = state_payload("error");
        s.message = Some(map_io_error(&e, &spec.part_path));
        emit_state(events, s);
        return;
    }
    let stat_len = fs::metadata(&spec.part_path).ok().map(|m| m.len());
    match (plan.size, stat_len) {
        (Some(expected), Some(actual)) => {
            if actual != expected {
                let mut s = state_payload("error");
                s.message = Some(format!(
                    "downloaded size mismatch: expected {}, got {}",
                    expected, actual
                ));
                emit_state(events, s);
                return;
            }
        }
        (_, None) => {
            let mut s = state_payload("error");
            s.message = Some(format!("could not stat partial file: {}", spec.part_path.display()));
            emit_state(events, s);
            return;
        }
        (None, Some(_)) => {}
    }
    let target = unique_target(&spec.final_path);
    if let Err(e) = fs::rename(&spec.part_path, &target) {
        let mut s = state_payload("error");
        s.message = Some(map_io_error(&e, &spec.final_path));
        emit_state(events, s);
        return;
    }
    let _ = fs::remove_file(&spec.meta_path);
    let final_size = fs::metadata(&target)
        .map(|m| m.len())
        .unwrap_or_else(|_| run.workers.iter().map(|w| w.done.load(Ordering::SeqCst)).sum());
    let mut s = state_payload("done");
    s.path = Some(target.to_string_lossy().into_owned());
    s.size = Some(final_size);
    emit_state(events, s);
}

/// First free rename target: "<stem> (k)<ext>" for k = 2, 3, ...
pub fn unique_target(final_path: &Path) -> PathBuf {
    if !final_path.exists() {
        return final_path.to_path_buf();
    }
    let parent = final_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let stem = final_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = final_path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for k in 2u64.. {
        let cand = parent.join(format!("{} ({}){}", stem, k, ext));
        if !cand.exists() {
            return cand;
        }
    }
    final_path.to_path_buf()
}

/// The engine supervisor: owns the whole download lifecycle and reports every
/// state change through the event channel. Never touches an AppHandle.
pub async fn run(
    spec: DownloadSpec,
    events: UnboundedSender<EngineEvent>,
    mut ctrl_rx: UnboundedReceiver<Ctrl>,
) {
    // ---- plan: sidecar resume or fresh ----
    let mut note: Option<String> = None;
    let mut plan = setup_plan(&spec, &mut note);

    // ---- part file + one-time preallocation ----
    let coord = match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&spec.part_path)
    {
        Ok(f) => f,
        Err(e) => {
            let mut s = state_payload("error");
            s.message = Some(map_io_error(&e, &spec.part_path));
            emit_state(&events, s);
            return;
        }
    };
    if let Some(size) = plan.size {
        if let Err(e) = coord.set_len(size) {
            let mut s = state_payload("error");
            s.message = Some(map_io_error(&e, &spec.part_path));
            emit_state(&events, s);
            return;
        }
    }
    let mut run = Arc::new(build_run(&plan));

    // ---- first state event + initial sidecar ----
    let mut s = state_payload("downloading");
    s.note = note;
    emit_state(&events, s);
    flush_meta(&spec, &plan, &run);

    let mut token = CancellationToken::new();
    let mut active = spawn_all(&spec, &plan, &run, &events, &token);
    if run.workers.is_empty() {
        finalize(&spec, &plan, &run, &events, &mut active, &coord).await;
        return;
    }

    let mut paused = false;
    loop {
        if paused {
            match ctrl_rx.recv().await {
                Some(Ctrl::Resume) => {
                    paused = false;
                    token = CancellationToken::new();
                    emit_state(&events, state_payload("downloading"));
                    active = spawn_all(&spec, &plan, &run, &events, &token);
                }
                Some(Ctrl::Cancel) => {
                    cleanup_cancel(&spec);
                    return;
                }
                Some(Ctrl::Pause) => {}
                None => return,
            }
            continue;
        }

        // The select handlers only build a decision; everything that mutates
        // supervisor-owned state runs after the borrow ends.
        enum Next {
            Completed,
            Fatal(FatalKind),
            WorkersGone,
            Pause,
            Cancel,
            Resume,
            Gone,
        }
        let next = tokio::select! {
            biased;
            ctrl = ctrl_rx.recv() => match ctrl {
                Some(Ctrl::Pause) => Next::Pause,
                Some(Ctrl::Cancel) => Next::Cancel,
                Some(Ctrl::Resume) => Next::Resume,
                None => Next::Gone,
            },
            msg = active.msgs_rx.recv() => match msg {
                Some(WorkerMsg::Complete) => Next::Completed,
                Some(WorkerMsg::Fatal(kind)) => Next::Fatal(kind),
                None => Next::WorkersGone,
            },
        };

        match next {
            Next::Resume => { /* already running */ }
            Next::Gone => return,
            Next::WorkersGone => {
                // All workers exited without completing or failing.
                let mut s = state_payload("error");
                s.message = Some("engine stopped unexpectedly".into());
                emit_state(&events, s);
                return;
            }
            Next::Completed => {
                active.completed += 1;
                if active.completed >= run.workers.len() {
                    finalize(&spec, &plan, &run, &events, &mut active, &coord).await;
                    return;
                }
            }
            Next::Pause => {
                active.token.cancel();
                join_bounded(&mut active.handles, JOIN_BUDGET).await;
                flush_meta(&spec, &plan, &run);
                let speed = run.speed_bps.load(Ordering::SeqCst);
                let _ = events.send(EngineEvent::Progress(build_progress(&run, plan.size, speed)));
                emit_state(&events, state_payload("paused"));
                paused = true;
            }
            Next::Cancel => {
                active.token.cancel();
                join_bounded(&mut active.handles, ERROR_JOIN_BUDGET).await;
                cleanup_cancel(&spec);
                return;
            }
            Next::Fatal(kind) => match kind {
                FatalKind::RefusedRangesStart if !plan.single && plan.size.is_some() => {
                    // GRACEFUL FALLBACK (CONTRACT): nothing written yet -> stop
                    // everyone, truncate the .part, switch to a single
                    // sequential connection, persist the meta, re-announce.
                    active.token.cancel();
                    join_bounded(&mut active.handles, ERROR_JOIN_BUDGET).await;
                    if let Err(e) = coord.set_len(0) {
                        let mut s = state_payload("error");
                        s.message = Some(map_io_error(&e, &spec.part_path));
                        emit_state(&events, s);
                        return;
                    }
                    plan = Plan::single(plan.size);
                    run = Arc::new(build_run(&plan));
                    flush_meta(&spec, &plan, &run);
                    let mut s = state_payload("downloading");
                    s.note = Some("server refused range requests - using a single connection".into());
                    emit_state(&events, s);
                    token = CancellationToken::new();
                    active = spawn_all(&spec, &plan, &run, &events, &token);
                }
                kind => {
                    active.token.cancel();
                    join_bounded(&mut active.handles, ERROR_JOIN_BUDGET).await;
                    let mut s = state_payload("error");
                    s.message = Some(kind.message());
                    emit_state(&events, s);
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hydra-unit-dl-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unique_target_naming() {
        let dir = temp_dir("unique");
        let target = dir.join("movie.mkv");
        assert_eq!(unique_target(&target), target);

        fs::write(&target, b"x").unwrap();
        let t2 = unique_target(&target);
        assert_eq!(t2, dir.join("movie (2).mkv"));

        fs::write(&t2, b"x").unwrap();
        assert_eq!(unique_target(&target), dir.join("movie (3).mkv"));

        let plain = dir.join("README");
        fs::write(&plain, b"x").unwrap();
        assert_eq!(unique_target(&plain), dir.join("README (2)"));

        let archive = dir.join("a.tar.gz");
        fs::write(&archive, b"x").unwrap();
        assert_eq!(unique_target(&archive), dir.join("a.tar (2).gz"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn io_error_mapping() {
        let e = io::Error::from_raw_os_error(28);
        assert_eq!(map_io_error(&e, Path::new("/d")), "disk full");
        let e = io::Error::from_raw_os_error(13);
        assert_eq!(
            map_io_error(&e, Path::new("/d")),
            "destination is not writable: /d"
        );
    }

    #[test]
    fn fresh_plan_shapes() {
        let spec = DownloadSpec {
            url: "u".into(),
            part_path: PathBuf::from("/tmp/hydra-unit/x.part"),
            meta_path: PathBuf::from("/tmp/hydra-unit/x.hydra.json"),
            final_path: PathBuf::from("/tmp/hydra-unit/x"),
            connections: 4,
            size: Some(10),
            probed_total: Some(10),
            accepts_ranges: true,
        };
        let mut note = None;
        let plan = setup_plan(&spec, &mut note);
        assert!(note.is_none());
        assert!(!plan.single);
        assert_eq!(plan.chunks.len(), 4);
        assert_eq!(plan.chunks[0].start, 0);
        assert_eq!(plan.chunks[0].len, Some(3));
        assert_eq!(plan.chunks[3].len, Some(1));
        assert_eq!(plan.chunk_size, 3);

        // No ranges -> single-stream plan with one full chunk.
        let spec2 = DownloadSpec { accepts_ranges: false, ..spec.clone() };
        let plan2 = setup_plan(&spec2, &mut note);
        assert!(plan2.single);
        assert_eq!(plan2.chunks.len(), 1);
        assert_eq!(plan2.chunks[0].len, Some(10));

        // Unknown size -> single-stream to EOF, no chunks metadata.
        let spec3 = DownloadSpec { size: None, ..spec };
        let plan3 = setup_plan(&spec3, &mut note);
        assert!(plan3.single);
        assert_eq!(plan3.chunks[0].len, None);
    }
}
