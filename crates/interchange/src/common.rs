//! Shared helpers: media paths and `file://` URLs, and decibel/gain conversions.

// ---------------------------------------------------------------------------------------------
// Paths and URLs
// ---------------------------------------------------------------------------------------------

fn is_windows_abs(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

pub(crate) fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with("\\\\") || is_windows_abs(p)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = h {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Convert a `file://` URL (`file:///a/b`, `file://localhost/a/b`, `file:///C:/x`) to a path.
/// Strings that are not file URLs are returned unchanged.
pub fn file_url_to_path(url: &str) -> String {
    let lower = url.get(..7).map(|s| s.to_ascii_lowercase());
    if lower.as_deref() != Some("file://") {
        return url.to_string();
    }
    let rest = &url[7..];
    let is_localhost = rest.get(..9).is_some_and(|s| s.eq_ignore_ascii_case("localhost"))
        && rest.get(9..).is_some_and(|after| after.is_empty() || after.starts_with('/'));
    let rest = if is_localhost { &rest[9..] } else { rest };
    let decoded = percent_decode(rest);
    // file:///C:/x -> C:/x
    if decoded.len() >= 3 && decoded.starts_with('/') && is_windows_abs(&decoded[1..]) {
        return decoded[1..].to_string();
    }
    if !decoded.starts_with('/') {
        // file://server/share -> UNC
        return format!("//{decoded}");
    }
    decoded
}

/// Convert an absolute path to a `file://` URL (`localhost` form for FCP7 XML).
pub fn path_to_file_url(path: &str, localhost: bool) -> String {
    let p = path.replace('\\', "/");
    // Windows verbatim paths (`\\?\C:\x`, `\\?\UNC\server\share`, as `canonicalize` returns them)
    // are not network shares: drop the prefix so `?` doesn't become the URL's host.
    let p = if let Some(unc) = p.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else if let Some(local) = p.strip_prefix("//?/") {
        local.to_string()
    } else {
        p
    };
    let host = if localhost { "localhost" } else { "" };
    if is_windows_abs(&p) {
        format!("file://{host}/{}", percent_encode_path(&p))
    } else if let Some(unc) = p.strip_prefix("//") {
        format!("file://{}", percent_encode_path(unc))
    } else {
        format!("file://{host}{}", percent_encode_path(&p))
    }
}

fn join(base: &str, rel: &str) -> String {
    let sep = if base.contains('\\') && !base.contains('/') { '\\' } else { '/' };
    let mut parts: Vec<&str> = base.split(['/', '\\']).collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    for seg in rel.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            s => parts.push(s),
        }
    }
    parts.join(&sep.to_string())
}

/// Resolve a media reference from a document: file URLs become paths, relative paths are joined to
/// `base_dir` (when given).
pub fn resolve_path(reference: &str, base_dir: Option<&str>) -> String {
    let p = file_url_to_path(reference.trim());
    if is_absolute(&p) {
        return p;
    }
    match base_dir {
        Some(b) if !b.is_empty() => join(b, &p),
        _ => p,
    }
}

/// Final path component.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// File name without extension.
pub(crate) fn file_stem(path: &str) -> &str {
    let n = file_name(path);
    match n.rfind('.') {
        Some(i) if i > 0 => &n[..i],
        _ => n,
    }
}

pub(crate) fn extension(path: &str) -> String {
    let n = file_name(path);
    n.rfind('.').map(|i| n[i + 1..].to_ascii_lowercase()).unwrap_or_default()
}

/// dB ↔ linear gain.
pub(crate) fn db_to_gain(db: f64) -> f64 {
    if db <= -287.0 { 0.0 } else { 10f64.powf(db / 20.0) }
}
pub(crate) fn gain_to_db(g: f64) -> f64 {
    if g <= 1e-15 { -287.5 } else { 20.0 * g.log10() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(file_url_to_path("file://localhost/Users/me/My%20Clip.mov"), "/Users/me/My Clip.mov");
        assert_eq!(file_url_to_path("file:///C:/Media/a.mxf"), "C:/Media/a.mxf");
        assert_eq!(file_url_to_path("/plain/path.mov"), "/plain/path.mov");
        assert_eq!(path_to_file_url("/Users/me/My Clip.mov", true), "file://localhost/Users/me/My%20Clip.mov");
        assert_eq!(path_to_file_url("C:\\Media\\a b.mxf", false), "file:///C:/Media/a%20b.mxf");
        assert_eq!(path_to_file_url("\\\\?\\C:\\Media\\a.mp4", true), "file://localhost/C:/Media/a.mp4");
        assert_eq!(path_to_file_url("\\\\?\\UNC\\srv\\share\\a.mp4", false), "file://srv/share/a.mp4");
        for p in ["/a/b c/ü.mov", "C:/x/y.mp4", "/r/%/#?.mov"] {
            assert_eq!(file_url_to_path(&path_to_file_url(p, true)), p);
            assert_eq!(file_url_to_path(&path_to_file_url(p, false)), p);
        }
    }

    #[test]
    fn file_url_localhost_prefix_host_is_kept() {
        assert_eq!(file_url_to_path("file://localhost-nas/share/a.mov"), "//localhost-nas/share/a.mov");
        assert_eq!(file_url_to_path("file://LOCALHOSTX/a"), "//LOCALHOSTX/a");
        assert_eq!(file_url_to_path("file://localhost/a/b"), "/a/b");
        assert_eq!(file_url_to_path("file://LocalHost/a"), "/a");
    }

    #[test]
    fn paths() {
        assert_eq!(resolve_path("media/a.mov", Some("/proj")), "/proj/media/a.mov");
        assert_eq!(resolve_path("../a.mov", Some("/proj/x/")), "/proj/a.mov");
        assert_eq!(resolve_path("/abs/a.mov", Some("/proj")), "/abs/a.mov");
        assert_eq!(file_stem("/x/y/Clip 01.final.mov"), "Clip 01.final");
    }
}
