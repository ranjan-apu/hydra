//! probe_url - CONTRACT "Commands / probe_url".
//!
//! HEAD first with User-Agent "Hydra/0.1":
//! - HEAD 200 with `Accept-Ranges: bytes` -> acceptsRanges=true, size=Content-Length,
//!   probeMethod="head".
//! - HEAD 200 WITHOUT the header -> one ranged GET ("Range: bytes=0-0") settles it:
//!   a 206 means the origin does honour ranges (upgrading the run from one
//!   connection to many), a 200 means it really is single-stream. probeMethod is
//!   "range" when that GET decided. If the verification itself fails, the HEAD
//!   conclusion stands rather than failing the probe.
//! - HEAD 403/405/501 -> the range fallback below.
//!
//! Range fallback (also used for the verification): a 206 + Content-Range
//! "bytes 0-0/TOTAL" gives acceptsRanges=true and size=TOTAL; a 200 means the
//! Range header was ignored. Only headers are read - the body is dropped
//! unread, so verifying never pulls the file.
//!
//! `server` carries the raw Server header of the response that decided the
//! result, so the UI can say what answered.

use reqwest::header::{
    ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE, SERVER,
};
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
        let final_url = head.url().to_string();
        let filename = names::resolve_filename(
            header_str(&head, CONTENT_DISPOSITION).as_deref(),
            &final_url,
        );
        let content_type = header_str(&head, CONTENT_TYPE);
        let server = header_str(&head, SERVER);

        if accept_ranges(&head) {
            return Ok(ProbeInfo {
                url: final_url,
                size,
                accepts_ranges: true,
                filename,
                content_type,
                probe_method: "head".into(),
                server,
            });
        }

        // Accept-Ranges is advisory: plenty of origins omit it on HEAD while
        // honouring Range on GET. One ranged GET is worth the round trip
        // because it can turn a single-stream run into a full-speed one.
        return match verify_ranges(url, client).await {
            Ok((accepts, total, verified_server)) => Ok(ProbeInfo {
                url: final_url,
                size: size.or(total),
                accepts_ranges: accepts,
                filename,
                content_type,
                probe_method: "range".into(),
                server: verified_server.or(server),
            }),
            // The verification is an optimisation, never a requirement.
            Err(_) => Ok(ProbeInfo {
                url: final_url,
                size,
                accepts_ranges: false,
                filename,
                content_type,
                probe_method: "head".into(),
                server,
            }),
        };
    }

    let code = status.as_u16();
    if code == 403 || code == 405 || code == 501 {
        return probe_by_range(url, client).await;
    }
    Err(format!("HTTP {}", code))
}

/// One ranged GET used purely for its headers.
/// Returns (accepts_ranges, total, server).
async fn verify_ranges(
    url: &str,
    client: &Client,
) -> Result<(bool, Option<u64>, Option<String>), String> {
    let resp = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|_| "could not connect to host".to_string())?;
    let status = resp.status();
    let server = header_str(&resp, SERVER);

    if status == StatusCode::PARTIAL_CONTENT {
        let total = header_str(&resp, CONTENT_RANGE)
            .as_deref()
            .and_then(plan::parse_content_range)
            .and_then(|cr| cr.total);
        return Ok((true, total, server));
    }
    if status == StatusCode::OK {
        // Range ignored: genuinely single-stream.
        return Ok((false, content_length(&resp), server));
    }
    Err(format!("HTTP {}", status.as_u16()))
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
            server: header_str(&resp, SERVER),
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
            server: header_str(&resp, SERVER),
        });
    }

    Err(format!("HTTP {}", status.as_u16()))
}

fn header_str(resp: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn content_length(resp: &reqwest::Response) -> Option<u64> {
    header_str(resp, CONTENT_LENGTH).and_then(|s| s.parse::<u64>().ok())
}

fn accept_ranges(resp: &reqwest::Response) -> bool {
    header_str(resp, ACCEPT_RANGES)
        .map(|v| v.to_ascii_lowercase().contains("bytes"))
        .unwrap_or(false)
}
