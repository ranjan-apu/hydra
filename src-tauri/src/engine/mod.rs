//! Hydra download engine - a tauri-free core.
//!
//! All wire-facing serde types use camelCase to match CONTRACT.md exactly.
//! The engine reports through a tokio mpsc channel of EngineEvent and never
//! touches an AppHandle; the thin bridge in src/bridge.rs owns the Tauri
//! integration, and src-tauri/examples/speedtest.rs drives the same core
//! headlessly.

pub mod download;
pub mod errors;
pub mod meta;
pub mod names;
pub mod plan;
pub mod probe;
pub mod settings;
pub mod types;

pub use download::{run, Ctrl, DownloadSpec};
pub use probe::probe;

use std::time::Duration;

/// Shared HTTP client shape (CONTRACT): rustls, HTTP/1.1 only, NO
/// gzip/brotli/deflate (an Accept-Encoding header would corrupt ranged byte
/// math), User-Agent "Hydra/0.1", 30 s connect timeout, no total timeout,
/// default redirect policy (workers use the final probe URL anyway).
///
/// The crate's reqwest is configured with default-features = false and
/// features = ["rustls-tls", "stream"], which already excludes the http2 and
/// compression features; http1_only() pins it explicitly.
/// One client for the whole process.
///
/// A probe and a download used to build their own client, so every run threw
/// away a warm connection pool and every TLS handshake had to happen again.
/// reqwest::Client is an Arc internally, so handing out clones is free.
pub fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent("Hydra/0.1")
                .http1_only()
                .connect_timeout(Duration::from_secs(30))
                .pool_idle_timeout(Duration::from_secs(90))
                .build()
                .expect("failed to build reqwest client")
        })
        .clone()
}
