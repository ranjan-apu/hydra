//! Filename resolution per CONTRACT "probe_url":
//! Content-Disposition `filename*` (RFC 5987, percent-decoded) preferred over
//! quoted `filename=`; else the URL path basename (query/fragment stripped,
//! percent-decoded). The chosen raw name is sanitized; a name that sanitizes
//! away to nothing becomes "download". Pure functions, hermetic tests.

/// Full resolution used by the probe: Content-Disposition first, URL basename
/// second, then sanitize.
pub fn resolve_filename(content_disposition: Option<&str>, url: &str) -> String {
    let raw = content_disposition
        .and_then(filename_from_content_disposition)
        .or_else(|| filename_from_url(url))
        .unwrap_or_default();
    sanitize_filename(&raw)
}

/// Split a Content-Disposition value into (lowercased-name, raw-value) params.
/// Honors quoted strings (a ';' inside quotes does not split) and backslash
/// escapes inside them (an escaped character is taken literally, quotes do not
/// toggle while escaped). Backslash-escape processing happens HERE, exactly
/// once - callers must not unescape again.
fn split_params(cd: &str) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = Vec::new();
    let mut name = String::new();
    let mut value = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    let mut have_eq = false;
    for ch in cd.chars() {
        if escaped {
            if have_eq {
                value.push(ch);
            } else {
                name.push(ch);
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => {
                in_quotes = !in_quotes;
                if have_eq {
                    value.push(ch);
                } else {
                    name.push(ch);
                }
            }
            '=' if !in_quotes && !have_eq => have_eq = true,
            ';' if !in_quotes => {
                params.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
                name.clear();
                value.clear();
                have_eq = false;
            }
            _ => {
                if have_eq {
                    value.push(ch);
                } else {
                    name.push(ch);
                }
            }
        }
    }
    params.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    params
}

/// Extract the filename from a Content-Disposition header value.
/// `filename*` (RFC 5987) is preferred over `filename=`.
pub fn filename_from_content_disposition(cd: &str) -> Option<String> {
    let params = split_params(cd);
    // RFC 5987: filename*=charset'lang'percent-encoded
    for (k, v) in &params {
        if k == "filename*" {
            if let Some(decoded) = decode_rfc5987(v) {
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
        }
    }
    for (k, v) in &params {
        if k == "filename" {
            // Quoted values keep their surrounding quotes in `v` (split_params
            // already resolved backslash escapes); strip them. Backslash
            // escapes were consumed while splitting, so a literal backslash in
            // the value stays a backslash.
            let inner = if let Some(stripped) = v.strip_prefix('"') {
                stripped.strip_suffix('"').unwrap_or(stripped)
            } else {
                v.as_str()
            };
            if !inner.is_empty() {
                return Some(inner.to_string());
            }
        }
    }
    None
}

/// Decode an RFC 5987 value: `charset'lang'pct-encoded`.
fn decode_rfc5987(v: &str) -> Option<String> {
    let first = v.find('\'')?;
    let charset = v[..first].trim().to_ascii_lowercase();
    let rest = &v[first + 1..];
    let second = rest.find('\'')?;
    let encoded = &rest[second + 1..];
    let bytes = percent_decode_bytes(encoded);
    if charset == "iso-8859-1" {
        Some(bytes.iter().map(|&b| b as char).collect())
    } else {
        // UTF-8 (the only charset the contract exercises) and anything else:
        // decode as UTF-8, lossily.
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Percent-decode into raw bytes; an invalid escape stays literal.
fn percent_decode_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(x) = hex_byte(b[i + 1], b[i + 2]) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn percent_decode_utf8(s: &str) -> String {
    String::from_utf8_lossy(&percent_decode_bytes(s)).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_byte(hi: u8, lo: u8) -> Option<u8> {
    Some((hex_val(hi)? << 4) | hex_val(lo)?)
}

/// Basename of the URL path: strip fragment then query, cut at the last '/',
/// percent-decode. None when there is no usable path segment.
pub fn filename_from_url(url: &str) -> Option<String> {
    let no_frag = url.split('#').next().unwrap_or(url);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    let after_scheme = match no_query.find("://") {
        Some(i) => &no_query[i + 3..],
        None => no_query,
    };
    // The path starts at the first '/' after the authority.
    let path_part = match after_scheme.find('/') {
        Some(i) => &after_scheme[i + 1..],
        None => "",
    };
    let base = path_part.rsplit('/').next().unwrap_or("");
    let decoded = percent_decode_utf8(base);
    let trimmed = decoded.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Remove path separators and control characters, trim; reject ".", ".." and
/// empty by substituting "download".
pub fn sanitize_filename(raw: &str) -> String {
    let filtered: String = raw
        .chars()
        .filter(|&c| c != '/' && c != '\\' && !c.is_ascii_control())
        .collect();
    let trimmed = filtered.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        "download".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_rfc5987_filename_star() {
        let cd = "attachment; filename=\"fallback.mkv\"; filename*=UTF-8''Reference%20File.mkv";
        assert_eq!(
            resolve_filename(Some(cd), "https://host/d/x.mkv"),
            "Reference File.mkv"
        );
    }

    #[test]
    fn rfc5987_percent_decodes_utf8() {
        let cd = "attachment; filename*=UTF-8''%E2%82%AC%20rate.bin";
        assert_eq!(filename_from_content_disposition(cd).unwrap(), "\u{20ac} rate.bin");
    }

    #[test]
    fn rfc5987_case_insensitive_charset() {
        let cd = "attachment; filename*=utf-8''a%20b.txt";
        assert_eq!(filename_from_content_disposition(cd).unwrap(), "a b.txt");
    }

    #[test]
    fn quoted_filename_with_escaped_quote() {
        // Raw header: filename="a\"b.tar.gz"  (backslash-escaped quote inside quotes.)
        let cd = "attachment; filename=\"a\\\"b.tar.gz\"";
        assert_eq!(filename_from_content_disposition(cd).unwrap(), "a\"b.tar.gz");
    }

    #[test]
    fn quoted_filename_with_escaped_backslash() {
        // Raw header: filename="a\\b.txt" -> literal value a\b.txt.
        let cd = "attachment; filename=\"a\\\\b.txt\"";
        assert_eq!(filename_from_content_disposition(cd).unwrap(), "a\\b.txt");
    }

    #[test]
    fn quoted_filename_with_semicolon() {
        let cd = "attachment; filename=\"a;b.txt\"; size=1";
        assert_eq!(filename_from_content_disposition(cd).unwrap(), "a;b.txt");
    }

    #[test]
    fn unquoted_token_filename() {
        let cd = "attachment; filename=plain-name.zip";
        assert_eq!(filename_from_content_disposition(cd).unwrap(), "plain-name.zip");
    }

    #[test]
    fn no_filename_param_is_none() {
        assert!(filename_from_content_disposition("attachment").is_none());
        assert!(filename_from_content_disposition("inline; size=2").is_none());
    }

    #[test]
    fn url_basename_rules() {
        assert_eq!(
            filename_from_url("https://h/d/Reference%20File.mkv?token=abc#frag").unwrap(),
            "Reference File.mkv"
        );
        assert_eq!(filename_from_url("https://h/d/x.bin").unwrap(), "x.bin");
        assert_eq!(filename_from_url("https://host/dl/file.mkv").unwrap(), "file.mkv");
        assert_eq!(filename_from_url("https://h/"), None);
        assert_eq!(filename_from_url("https://h/d/"), None);
        assert_eq!(filename_from_url("https://host"), None);
        assert_eq!(filename_from_url("file:///tmp/x.bin").unwrap(), "x.bin");
        // '+' is not a space in paths.
        assert_eq!(filename_from_url("https://h/a+b.txt").unwrap(), "a+b.txt");
    }

    #[test]
    fn sanitize_rules() {
        assert_eq!(sanitize_filename("a/b\\c.txt"), "abc.txt");
        assert_eq!(sanitize_filename("na\u{1}me.txt"), "name.txt");
        assert_eq!(sanitize_filename("  spaced  "), "spaced");
        assert_eq!(sanitize_filename("."), "download");
        assert_eq!(sanitize_filename(".."), "download");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("   "), "download");
        // Separators are removed; remaining dots stay (no traversal is
        // possible without separators). Only exact "." / ".." / empty fall
        // back to "download", per CONTRACT.
        assert_eq!(sanitize_filename("../.."), "....");
        assert_eq!(sanitize_filename("../../etc/passwd"), "....etcpasswd");
        assert_eq!(sanitize_filename("normal.file.mkv"), "normal.file.mkv");
    }

    #[test]
    fn resolve_falls_back_to_url_then_download() {
        assert_eq!(
            resolve_filename(None, "https://host/d/file%201.mkv"),
            "file 1.mkv"
        );
        assert_eq!(resolve_filename(None, "https://host/"), "download");
        assert_eq!(resolve_filename(Some("attachment"), "https://host/"), "download");
    }
}
