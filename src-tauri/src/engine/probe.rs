//! probe_url - CONTRACT "Commands / probe_url".
//!
//! HEAD first with User-Agent "Hydra/0.1"; on 403/405/501 fall back to a
//! ranged GET ("Range: bytes=0-0"):
//! - 206 + Content-Range "bytes 0-0/TOTAL" -> acceptsRanges=true, size=TOTAL,
//!   probeMethod="range".
//! - HEAD 200 -> size=Content-Length (null if absent), acceptsRanges from the
//!   Accept-Ranges header, probeMethod="head".
//! - ranged GET -> 200 (Range ignored) -> acceptsRanges=false, size from
//!   Content-Length, probeMethod="range".
//! Errors: "could not connect to host", "HTTP <code>".

use reqwest::header::{ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use reqwest::{Client, StatusCode};

use super::names;
use super::plan;
use super::types::ProbeInfo;

/// Probe a URL and resolve size / range support / filename.
pub async fn probe(url: &str, client: &Client) -> Result<ProbeInfo, String> {
    let head = client
        .head(url)
        .send()
        .await
        .map_err(|_| "could not connect to host".to_string())?;
    let status = head.status();
    if status == StatusCode::OK {
        let size = content_length(&head);
        let accepts = accept_ranges(&head);
        let final_url = head.url().to_string();
        let filename =
            names::resolve_filename(header_str(&head, CONTENT_DISPOSITION).as_deref(), &final_url);
        let content_type = header_str(&head, CONTENT_TYPE);
        return Ok(ProbeInfo {
            url: final_url,
            size,
            accepts_ranges: accepts,
            filename,
            content_type,
            probe_method: "head".into(),
        });
    }
    let code = status.as_u16();
    if code == 403 || code == 405 || code == 501 {
        return probe_by_range(url, client).await;
    }
    Err(format!("HTTP {}", code))
}

/// The 403/405/501 fallback: one ranged GET for bytes 0-0.
async fn probe_by_range(url: &str, client: &Client) -> Result<ProbeInfo, String> {
    let resp = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|_| "could not connect to host".to_string())?;
    let status = resp.status();
    if status == StatusCode::PARTIAL_CONTENT {
        // Content-Range "bytes 0-0/TOTAL" (or "/*" when the total is unknown).
        let total = header_str(&resp, CONTENT_RANGE)
            .as_deref()
            .and_then(plan::parse_content_range)
            .and_then(|cr| cr.total);
        let final_url = resp.url().to_string();
        let filename = names::resolve_filename(
            header_str(&resp, CONTENT_DISPOSITION).as_deref(),
            &final_url,
        );
        let content_type = header_str(&resp, CONTENT_TYPE);
        return Ok(ProbeInfo {
            url: final_url,
            size: total,
            accepts_ranges: true,
            filename,
            content_type,
            probe_method: "range".into(),
        });
    }
    if status == StatusCode::OK {
        // Range ignored: single-stream only.
        let size = content_length(&resp);
        let final_url = resp.url().to_string();
        let filename = names::resolve_filename(
            header_str(&resp, CONTENT_DISPOSITION).as_deref(),
            &final_url,
        );
        let content_type = header_str(&resp, CONTENT_TYPE);
        return Ok(ProbeInfo {
            url: final_url,
            size,
            accepts_ranges: false,
            filename,
            content_type,
            probe_method: "range".into(),
        });
    }
    Err(format!("HTTP {}", status.as_u16()))
}

fn header_str(resp: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
}

fn content_length(resp: &reqwest::Response) -> Option<u64> {
    header_str(resp, CONTENT_LENGTH).and_then(|s| s.parse::<u64>().ok())
}

fn accept_ranges(resp: &reqwest::Response) -> bool {
    header_str(resp, ACCEPT_RANGES)
        .map(|v| v.to_ascii_lowercase().contains("bytes"))
        .unwrap_or(false)
}
