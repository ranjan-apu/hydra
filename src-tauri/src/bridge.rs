//! The thin Tauri bridge: every CONTRACT command, the download registry
//! (managed state), and the forwarder that turns EngineEvents into
//! "hydra://state" / "hydra://progress" app events.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::mpsc::{channel, unbounded_channel, Receiver, UnboundedSender};

use crate::engine::settings::Settings;
use crate::engine::types::{EngineEvent, ProbeInfo, ProgressPayload, StatePayload};
use crate::engine::{self, names, Ctrl, DownloadSpec};

// ---------------------------------------------------------------- registry

/// Managed state: live download handles keyed by "dl-<n>".
pub struct Registry {
    inner: Mutex<HashMap<String, Handle>>,
    next: AtomicU64,
    /// Serialises settings writes: load-modify-save over a shared temp file must
    /// not interleave, or one partial update silently drops the other field.
    settings_lock: tokio::sync::Mutex<()>,
}

/// Cap on simultaneously running downloads. Each run holds a coordinator fd plus
/// one fd per worker, so an unbounded number of them is an fd/RAM exhaustion
/// path. The UI never gets near this; it exists because IPC is a trust boundary.
const MAX_CONCURRENT_DOWNLOADS: usize = 8;

#[derive(Clone)]
struct Handle {
    ctrl: UnboundedSender<Ctrl>,
    /// The .part this run owns, so a second run cannot target the same file.
    part_path: PathBuf,
}

impl Registry {
    pub fn new() -> Self {
        Registry {
            inner: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            settings_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn alloc_id(&self) -> String {
        format!("dl-{}", self.next.fetch_add(1, Ordering::SeqCst))
    }

    fn insert(&self, id: String, handle: Handle) {
        self.inner.lock().unwrap().insert(id, handle);
    }

    fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    fn get(&self, id: &str) -> Option<Handle> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// True when a live run already owns this destination file.
    fn owns_path(&self, part_path: &std::path::Path) -> bool {
        self.inner
            .lock()
            .unwrap()
            .values()
            .any(|h| h.part_path == part_path)
    }
}

// ------------------------------------------------------------ wire payloads

/// "id" + the engine state payload, flattened.
#[derive(Clone, Serialize)]
struct WireState {
    id: String,
    #[serde(flatten)]
    inner: StatePayload,
}

#[derive(Clone, Serialize)]
struct WireProgress {
    id: String,
    #[serde(flatten)]
    inner: ProgressPayload,
}

/// { "id": "dl-1" } return of start_download.
#[derive(Debug, Clone, Serialize)]
pub struct StartAck {
    pub id: String,
}

/// meta_check return.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaCheck {
    pub exists: bool,
    pub compatible: bool,
    pub bytes_done: u64,
}

// ----------------------------------------------------------------- commands

/// CONTRACT probe_url.
#[tauri::command]
pub async fn probe_url(url: String) -> Result<ProbeInfo, String> {
    let client = engine::http_client();
    engine::probe(&url, &client).await
}

/// CONTRACT start_download: { url, destDir, filename, connections, size,
/// acceptsRanges } -> { id }. Emits hydra://state "downloading" (with note
/// when resuming or falling back) then hydra://progress at >= 4 Hz.
#[tauri::command]
pub async fn start_download(
    app: AppHandle,
    url: String,
    dest_dir: String,
    filename: String,
    connections: u64,
    size: Option<u64>,
    accepts_ranges: bool,
) -> Result<StartAck, String> {
    let connections = connections.clamp(1, 16) as usize;
    let filename = names::sanitize_filename(&filename);
    let dir = PathBuf::from(&dest_dir);
    check_writable(&dir)?;

    let registry = app.state::<Registry>();
    // IPC is a trust boundary: nothing stops a caller from asking for twenty
    // simultaneous downloads, and each one costs a coordinator fd plus a worker
    // fd per connection. Cap the count and refuse a second run on the same file.
    let part_path = dir.join(format!("{}.part", filename));
    if registry.len() >= MAX_CONCURRENT_DOWNLOADS {
        return Err("too many downloads in progress".to_string());
    }
    if registry.owns_path(&part_path) {
        return Err("that file is already downloading".to_string());
    }
    let id = registry.alloc_id();
    let spec = DownloadSpec {
        url,
        part_path: part_path.clone(),
        meta_path: dir.join(format!("{}.hydra.json", filename)),
        final_path: dir.join(&filename),
        connections,
        // The app always downloads the whole probed file: the download size
        // and the Content-Range cross-check reference are the same value.
        size,
        probed_total: size,
        accepts_ranges,
    };

    // Bounded: a stalled webview must not let the event queue grow without
    // limit. Progress frames are dropped when it fills; state frames wait.
    let (evt_tx, evt_rx) = channel::<EngineEvent>(engine::download::EVENT_QUEUE_CAP);
    let (ctrl_tx, ctrl_rx) = unbounded_channel::<Ctrl>();
    tokio::spawn(engine::run(spec, evt_tx, ctrl_rx));
    registry.insert(
        id.clone(),
        Handle {
            ctrl: ctrl_tx,
            part_path,
        },
    );
    spawn_forwarder(app, id.clone(), evt_rx);
    Ok(StartAck { id })
}

/// CONTRACT pause_download: stop workers, flush meta, emit "paused"; keeps
/// .part + meta + the in-memory handle for fast resume.
#[tauri::command]
pub async fn pause_download(app: AppHandle, id: String) -> Result<(), String> {
    let registry = app.state::<Registry>();
    let handle = registry
        .get(&id)
        .ok_or_else(|| format!("unknown download: {}", id))?;
    handle
        .ctrl
        .send(Ctrl::Pause)
        .map_err(|_| format!("download is not active: {}", id))?;
    Ok(())
}

/// CONTRACT resume_download: re-spawn workers from in-memory state.
#[tauri::command]
pub async fn resume_download(app: AppHandle, id: String) -> Result<(), String> {
    let registry = app.state::<Registry>();
    let handle = registry
        .get(&id)
        .ok_or_else(|| format!("unknown download: {}", id))?;
    handle
        .ctrl
        .send(Ctrl::Resume)
        .map_err(|_| format!("download is not active: {}", id))?;
    Ok(())
}

/// CONTRACT cancel_download: stop workers, delete .part + sidecar, forget the
/// download. No state event afterwards.
#[tauri::command]
pub async fn cancel_download(app: AppHandle, id: String) -> Result<(), String> {
    let registry = app.state::<Registry>();
    let Some(handle) = registry.get(&id) else {
        return Ok(());
    };
    // Ask first, forget only once the engine confirms the files are gone: the UI
    // immediately re-checks for a resumable partial, and an immediate restart on
    // the same filename must not be able to race the cleanup sweep.
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel::<()>();
    let sent = handle.ctrl.send(Ctrl::Cancel(Some(ack_tx))).is_ok();
    if sent {
        // Bounded wait: a wedged engine must not wedge the IPC call.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), ack_rx).await;
    }
    registry.remove(&id);
    Ok(())
}

/// CONTRACT pick_folder: native folder picker, callback (non-blocking) form,
/// opening at the remembered folder when one is set.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    let mut builder = app.dialog().file();
    if let Some(dir) = engine::settings::load(&settings_path(&app)).dest_dir {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            builder = builder.set_directory(p);
        }
    }
    builder.pick_folder(move |picked| {
        let out = picked
            .and_then(|f| f.into_path().ok())
            .map(|p| p.to_string_lossy().into_owned());
        let _ = tx.send(out);
    });
    rx.await
        .map_err(|_| "could not open folder picker".to_string())
}

/// CONTRACT reveal_path: Finder reveal of the item.
#[tauri::command]
pub async fn reveal_path(app: AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|_| "could not reveal that file".to_string())
}

/// CONTRACT get_settings.
#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    Ok(engine::settings::load(&settings_path(&app)))
}

/// CONTRACT save_settings - partial update:
/// destDir absent = keep, null = clear, string = set; connections absent =
/// keep, number = set (clamped 1..=16). Nested Option<Option<_>> distinguishes
/// a missing key from an explicit null at the IPC deserializer level.
#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    dest_dir: Option<Option<String>>,
    connections: Option<u64>,
) -> Result<(), String> {
    let registry = app.state::<Registry>();
    let _guard = registry.settings_lock.lock().await;
    let path = settings_path(&app);
    let mut current = engine::settings::load(&path);
    // clamp in u64 first: casting a huge value to u32 would wrap before the
    // clamp ever saw it
    engine::settings::apply(
        &mut current,
        dest_dir,
        connections.map(|c| c.clamp(1, 16) as u32),
    );
    engine::settings::save(&path, &current).map_err(|_| "could not save settings".to_string())
}

/// CONTRACT meta_check: whether a resumable sidecar matching url+size exists.
#[tauri::command]
pub async fn meta_check(
    url: String,
    dest_dir: String,
    filename: String,
    size: Option<u64>,
) -> Result<MetaCheck, String> {
    let dir = PathBuf::from(dest_dir);
    // same sanitizer as start_download: a crafted filename must not be able to
    // point the sidecar lookup outside the destination directory
    let filename = names::sanitize_filename(&filename);
    let meta_path = dir.join(format!("{}.hydra.json", filename));
    let part_path = dir.join(format!("{}.part", filename));
    if !meta_path.exists() || !part_path.exists() {
        return Ok(MetaCheck {
            exists: false,
            compatible: false,
            bytes_done: 0,
        });
    }
    let size = match size {
        Some(s) => s,
        None => {
            return Ok(MetaCheck {
                exists: true,
                compatible: false,
                bytes_done: 0,
            })
        }
    };
    match engine::meta::load(&meta_path) {
        Some(mut m) if engine::meta::is_compatible(&m, &url, size) => {
            let part_len = std::fs::metadata(&part_path).map(|s| s.len()).unwrap_or(0);
            engine::meta::clamp_dones(&mut m, part_len);
            let done = engine::meta::bytes_done(&m);
            Ok(MetaCheck {
                exists: true,
                compatible: true,
                bytes_done: done,
            })
        }
        _ => Ok(MetaCheck {
            exists: true,
            compatible: false,
            bytes_done: 0,
        }),
    }
}

// ------------------------------------------------------------------ helpers

/// Forward EngineEvents to the frontend, injecting the download id; forgets
/// the handle after a terminal state.
fn spawn_forwarder(app: AppHandle, id: String, mut rx: Receiver<EngineEvent>) {
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                EngineEvent::State(s) => {
                    let terminal = s.terminal();
                    let _ = app.emit(
                        "hydra://state",
                        WireState {
                            id: id.clone(),
                            inner: s,
                        },
                    );
                    if terminal {
                        app.state::<Registry>().remove(&id);
                        break;
                    }
                }
                EngineEvent::Progress(p) => {
                    let _ = app.emit(
                        "hydra://progress",
                        WireProgress {
                            id: id.clone(),
                            inner: p,
                        },
                    );
                }
            }
        }
    });
}

fn settings_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .map(|d| d.join("settings.json"))
        .unwrap_or_else(|_| PathBuf::from("settings.json"))
}

/// Destination check: an existing directory we can create files in. Human
/// lowercase error strings per CONTRACT, produced by the shared error mapper so
/// a disk-full or permission failure reads the same here as it does mid-transfer.
fn check_writable(dir: &std::path::Path) -> Result<(), String> {
    match std::fs::metadata(dir) {
        Err(_) => return Err(format!("destination is not writable: {}", dir.display())),
        Ok(md) if !md.is_dir() => {
            return Err(format!("destination is not writable: {}", dir.display()));
        }
        Ok(_) => {}
    }
    // A unique name reserved with create_new: a user file that happens to be
    // called .hydra-write-probe is never truncated, and two concurrent checks
    // cannot collide.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let probe = dir.join(format!(
        ".hydra-write-probe-{}-{}",
        std::process::id(),
        nanos
    ));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Err(e) => Err(engine::errors::write_error(Some(dir), &e)),
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::types::ConnInfo;

    #[test]
    fn state_event_wire_shape() {
        // done: id + state + path + size (message/note omitted)
        let mut s = state_like("done");
        s.path = Some("/dl/movie.mkv".into());
        s.size = Some(7772412965);
        let json = serde_json::to_string(&WireState {
            id: "dl-1".into(),
            inner: s,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"id":"dl-1","state":"done","path":"/dl/movie.mkv","size":7772412965}"#
        );
        // error: id + state + message
        let mut e = state_like("error");
        e.message = Some("HTTP 403".into());
        let j = serde_json::to_string(&WireState {
            id: "dl-2".into(),
            inner: e,
        })
        .unwrap();
        assert_eq!(j, r#"{"id":"dl-2","state":"error","message":"HTTP 403"}"#);
        // downloading with optional note
        let mut d = state_like("downloading");
        d.note = Some("Resuming partial download".into());
        let j = serde_json::to_string(&WireState {
            id: "dl-3".into(),
            inner: d,
        })
        .unwrap();
        assert_eq!(
            j,
            r#"{"id":"dl-3","state":"downloading","note":"Resuming partial download"}"#
        );
        let mut f = state_like("downloading");
        f.note = Some("server refused range requests - using a single connection".into());
        let j = serde_json::to_string(&WireState {
            id: "dl-4".into(),
            inner: f,
        })
        .unwrap();
        assert_eq!(
            j,
            r#"{"id":"dl-4","state":"downloading","note":"server refused range requests - using a single connection"}"#
        );
    }

    fn state_like(state: &str) -> StatePayload {
        StatePayload {
            state: state.to_string(),
            path: None,
            size: None,
            message: None,
            note: None,
        }
    }

    #[test]
    fn progress_event_wire_shape() {
        let conns = vec![
            ConnInfo {
                index: 0,
                done: 1234567,
                size: Some(1943103242),
                active: true,
                phase: "receiving".into(),
            },
            ConnInfo {
                index: 1,
                done: 0,
                size: None,
                active: false,
                phase: "idle".into(),
            },
        ];
        let p = ProgressPayload {
            bytes_done: 1234567,
            total: Some(7772412965),
            speed_bps: 27000000,
            eta_secs: Some(245),
            conns,
        };
        let json = serde_json::to_string(&WireProgress {
            id: "dl-1".into(),
            inner: p,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"id":"dl-1","bytesDone":1234567,"total":7772412965,"speedBps":27000000,"etaSecs":245,"conns":[{"index":0,"done":1234567,"size":1943103242,"active":true,"phase":"receiving"},{"index":1,"done":0,"size":null,"active":false,"phase":"idle"}]}"#
        );
        // Unknown total / zero speed: keys stay present with null.
        let p = ProgressPayload {
            bytes_done: 5,
            total: None,
            speed_bps: 0,
            eta_secs: None,
            conns: vec![],
        };
        let j = serde_json::to_string(&WireProgress {
            id: "dl-9".into(),
            inner: p,
        })
        .unwrap();
        assert_eq!(
            j,
            r#"{"id":"dl-9","bytesDone":5,"total":null,"speedBps":0,"etaSecs":null,"conns":[]}"#
        );
    }

    #[test]
    fn ack_shapes() {
        assert_eq!(
            serde_json::to_string(&StartAck { id: "dl-1".into() }).unwrap(),
            r#"{"id":"dl-1"}"#
        );
        let m = MetaCheck {
            exists: true,
            compatible: true,
            bytes_done: 42,
        };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"exists":true,"compatible":true,"bytesDone":42}"#
        );
    }
}
