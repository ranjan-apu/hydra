//! Wire-facing types (CONTRACT.md JSON shapes, camelCase).
//!
//! Everything the engine reports crosses a process boundary as JSON, so every
//! struct here uses #[serde(rename_all = "camelCase")] and mirrors CONTRACT.md
//! exactly. The engine itself is tauri-free: it emits `EngineEvent`s through a
//! tokio mpsc channel and never touches an AppHandle.

use serde::Serialize;

/// Result of the `probe_url` command - CONTRACT "Commands / probe_url".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeInfo {
    /// FINAL resolved URL (after redirects); workers do not re-follow them.
    pub url: String,
    /// null when the server did not tell us.
    pub size: Option<u64>,
    pub accepts_ranges: bool,
    pub filename: String,
    /// Raw Content-Type header value, null when absent.
    pub content_type: Option<String>,
    /// "head" | "range".
    pub probe_method: String,
}

/// One connection's status inside a `hydra://progress` payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnInfo {
    /// Connection index; worker i owns chunk i for the whole run.
    pub index: usize,
    pub done: u64,
    /// Chunk byte size, null when the total is unknown.
    pub size: Option<u64>,
    pub active: bool,
    /// "idle" | "connecting" | "receiving" | "retrying" | "done".
    pub phase: String,
}

/// `hydra://progress` payload (without the `id`, added by the bridge).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPayload {
    pub bytes_done: u64,
    /// null when unknown.
    pub total: Option<u64>,
    /// Rolling ~2 s window, bytes per second.
    pub speed_bps: u64,
    /// null when total unknown or speed ~ 0.
    pub eta_secs: Option<u64>,
    /// One entry per active connection.
    pub conns: Vec<ConnInfo>,
}

/// `hydra://state` payload (without the `id`, added by the bridge).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatePayload {
    /// "downloading" | "paused" | "done" | "error".
    pub state: String,
    /// state=done only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// state=done only (verified byte count).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// state=error only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Optional, any state ("Resuming partial download",
    /// "server refused range requests - using a single connection").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl StatePayload {
    /// "done" and "error" are terminal: the bridge forgets the handle after them.
    pub fn terminal(&self) -> bool {
        matches!(self.state.as_str(), "done" | "error")
    }
}

/// Everything the engine reports. The bridge forwards these to Tauri events
/// ("hydra://state" / "hydra://progress"); the headless speedtest harness
/// prints them.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    State(StatePayload),
    Progress(ProgressPayload),
}

/// Worker phases, shared through an AtomicU8.
pub const PHASE_IDLE: u8 = 0;
pub const PHASE_CONNECTING: u8 = 1;
pub const PHASE_RECEIVING: u8 = 2;
pub const PHASE_RETRYING: u8 = 3;
pub const PHASE_DONE: u8 = 4;

/// Wire string for a phase value.
pub fn phase_str(phase: u8) -> &'static str {
    match phase {
        PHASE_CONNECTING => "connecting",
        PHASE_RECEIVING => "receiving",
        PHASE_RETRYING => "retrying",
        PHASE_DONE => "done",
        _ => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_info_wire_shape() {
        let p = ProbeInfo {
            url: "https://final.example/dld/file.mkv".into(),
            size: Some(7772412965),
            accepts_ranges: true,
            filename: "reference-file.mkv".into(),
            content_type: Some("application/octet-stream".into()),
            probe_method: "head".into(),
        };
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(
            j,
            r#"{"url":"https://final.example/dld/file.mkv","size":7772412965,"acceptsRanges":true,"filename":"reference-file.mkv","contentType":"application/octet-stream","probeMethod":"head"}"#
        );
        let u = ProbeInfo {
            url: "u".into(),
            size: None,
            accepts_ranges: false,
            filename: "d".into(),
            content_type: None,
            probe_method: "range".into(),
        };
        assert_eq!(
            serde_json::to_string(&u).unwrap(),
            r#"{"url":"u","size":null,"acceptsRanges":false,"filename":"d","contentType":null,"probeMethod":"range"}"#
        );
    }

    #[test]
    fn state_terminal_detection() {
        assert!(state_like("done").terminal());
        assert!(state_like("error").terminal());
        assert!(!state_like("downloading").terminal());
        assert!(!state_like("paused").terminal());
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
}
