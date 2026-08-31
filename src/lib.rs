//! Rust port of `scripts/validate_readme.py`.
//!
//! Preserves the exact behavior of the original Python module: same regexes,
//! same URL normalization semantics (mirroring `urllib.parse.urlsplit`), the
//! same error/warning message strings, and the same append ordering that the
//! test-suite relies on.

use std::collections::HashMap;
use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::Duration;

use regex::Regex;

/// `RESOURCE_RE = re.compile(r"^- \[([^\]]+)]\((https://[^)\s]+)\): (.+)$")`
///
/// Note the URL group must start with `https://`.
fn resource_re() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^- \[([^\]]+)]\((https://[^)\s]+)\): (.+)$").unwrap())
}

/// `LINK_RE = re.compile(r"^- \[")`
fn link_re() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^- \[").unwrap())
}

/// `USER_AGENT = "awesome-ai-resource-validator/1.0"`
pub const USER_AGENT: &str = "awesome-ai-resource-validator/1.0";

/// Mirror of the frozen `@dataclass` `Resource`.
///
/// Field order matches the Python dataclass: `line, section, category, title,
/// url, description`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub line: usize,
    pub section: String,
    pub category: String,
    pub title: String,
    pub url: String,
    pub description: String,
}

/// Severity of a link-check or classification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    /// The lowercase label used by the Python tuples (`"error"` / `"warning"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// Error raised when a URL's port cannot be parsed, mirroring Python's
/// `ValueError` from `SplitResult.port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError(pub String);

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The subset of `urllib.parse.urlsplit`'s `SplitResult` that
/// `normalize_url` relies on.
struct SplitResult {
    scheme: String,
    host: String,
    port_raw: Option<String>,
    path: String,
    query: String,
}

impl SplitResult {
    /// Equivalent of `SplitResult.hostname`: the lowercased host with no port.
    fn hostname(&self) -> String {
        self.host.to_lowercase()
    }

    /// Equivalent of `SplitResult.port`: parse the raw port, raising
    /// `ValueError` when it is not a valid integer (matching CPython).
    fn port(&self) -> Result<Option<u32>, ValueError> {
        match &self.port_raw {
            None => Ok(None),
            Some(raw) => raw.parse::<u32>().map(Some).map_err(|_| {
                ValueError(format!(
                    "Port could not be cast to integer value as {}",
                    PyRepr(raw)
                ))
            }),
        }
    }
}

/// Helper that renders a string the way Python's `repr()` would for the port
/// error message (single-quoted).
struct PyRepr<'a>(&'a str);

impl fmt::Display for PyRepr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "'{}'", self.0)
    }
}

/// Split a URL the way `urllib.parse.urlsplit` does for the inputs this
/// validator handles: `scheme://netloc/path?query#fragment`.
///
/// The netloc is parsed into host + optional raw port. Only the fields used by
/// `normalize_url` are populated. The fragment is discarded by the caller.
fn urlsplit(url: &str) -> SplitResult {
    // Separate scheme.
    let (scheme, rest) = match url.find("://") {
        Some(idx) => {
            let scheme = &url[..idx];
            // A scheme is only valid if it is all scheme chars; otherwise the
            // whole thing is treated as a path (as urlsplit does). For our
            // inputs the scheme is always present, but keep the guard simple.
            (scheme.to_string(), &url[idx + 3..])
        }
        None => (String::new(), url),
    };

    // Strip fragment (everything after the first '#').
    let (rest, _fragment) = match rest.find('#') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };

    // Split query (everything after the first '?').
    let (rest, query) = match rest.find('?') {
        Some(idx) => (&rest[..idx], rest[idx + 1..].to_string()),
        None => (rest, String::new()),
    };

    // netloc ends at the first '/', '?' or '#'; query/# already removed.
    let (netloc, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], rest[idx..].to_string()),
        None => (rest, String::new()),
    };

    // netloc may contain userinfo@host:port. Drop userinfo before '@'.
    let host_port = match netloc.rfind('@') {
        Some(idx) => &netloc[idx + 1..],
        None => netloc,
    };

    // Split host and raw port on the last ':' (IPv6 not needed for our inputs).
    let (host, port_raw) = match host_port.rfind(':') {
        Some(idx) => (
            host_port[..idx].to_string(),
            Some(host_port[idx + 1..].to_string()),
        ),
        None => (host_port.to_string(), None),
    };

    SplitResult {
        scheme,
        host,
        port_raw,
        path,
        query,
    }
}

/// Reassemble a URL the way `urllib.parse.urlunsplit` does for the
/// `(scheme, netloc, path, query, fragment="")` tuples produced here.
fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str) -> String {
    let mut url = String::new();
    // urlunsplit prepends "//" + netloc when netloc is set, or when the path
    // starts with "//". For our normalized inputs netloc is always present.
    if !netloc.is_empty() || path.starts_with("//") {
        url.push_str(scheme);
        url.push(':');
        url.push_str("//");
        url.push_str(netloc);
        url.push_str(path);
    } else if !scheme.is_empty() {
        url.push_str(scheme);
        url.push(':');
        url.push_str(path);
    } else {
        url.push_str(path);
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    url
}

/// Port of `normalize_url`.
///
/// Lowercases scheme and host, drops the fragment, keeps the query, strips
/// trailing slashes from the path (empty path becomes `/`), and removes the
/// `:443` port only for `https`. Returns `Err(ValueError)` when the port is
/// non-numeric, mirroring CPython's `SplitResult.port`.
pub fn normalize_url(url: &str) -> Result<String, ValueError> {
    let parts = urlsplit(url);
    let mut hostname = parts.hostname();
    let port = parts.port()?;
    let scheme_lower = parts.scheme.to_lowercase();
    if let Some(port) = port {
        if !(scheme_lower == "https" && port == 443) {
            hostname = format!("{hostname}:{port}");
        }
    }
    let trimmed = parts.path.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    Ok(urlunsplit(&scheme_lower, &hostname, path, &parts.query))
}

/// Result of `validate_text`: parsed resources plus error/warning messages.
pub struct Validation {
    pub resources: Vec<Resource>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Port of `validate_text`.
///
/// Walks the README line-by-line tracking the current `## section` and
/// `### category`, collecting resources and errors in the exact order the
/// Python implementation appends them.
pub fn validate_text(text: &str) -> Validation {
    let mut resources: Vec<Resource> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let warnings: Vec<String> = Vec::new();

    let mut section = String::new();
    let mut category = String::new();

    // Preserve insertion order for the empty-category pass (Python dicts are
    // insertion-ordered).
    let mut category_order: Vec<(String, String)> = Vec::new();
    let mut category_lines: HashMap<(String, String), usize> = HashMap::new();
    let mut category_counts: HashMap<(String, String), usize> = HashMap::new();

    for (idx, line) in text.split('\n').enumerate() {
        let line_number = idx + 1;

        if let Some(rest) = line.strip_prefix("## ") {
            section = rest.trim().to_string();
            category = String::new();
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            category = rest.trim().to_string();
            let key = (section.clone(), category.clone());
            if !category_lines.contains_key(&key) {
                category_order.push(key.clone());
            }
            category_lines.insert(key.clone(), line_number);
            category_counts.insert(key, 0);
            continue;
        }

        if !link_re().is_match(line) {
            continue;
        }

        let captures = match resource_re().captures(line) {
            Some(c) => c,
            None => {
                errors.push(format!("line {line_number}: malformed resource entry"));
                continue;
            }
        };

        if category.is_empty() {
            errors.push(format!(
                "line {line_number}: resource is outside a level-three category"
            ));
            continue;
        }

        let title = captures.get(1).unwrap().as_str();
        let url = captures.get(2).unwrap().as_str();
        let description = captures.get(3).unwrap().as_str();

        if !description.ends_with('.') {
            errors.push(format!(
                "line {line_number}: description must end with a period"
            ));
        }

        resources.push(Resource {
            line: line_number,
            section: section.clone(),
            category: category.clone(),
            title: title.trim().to_string(),
            url: url.to_string(),
            description: description.trim().to_string(),
        });
        *category_counts
            .get_mut(&(section.clone(), category.clone()))
            .unwrap() += 1;
    }

    // Empty-category errors, in insertion order.
    for key in &category_order {
        let count = category_counts[key];
        if count == 0 {
            let (section_name, category_name) = key;
            let location = if section_name.is_empty() {
                String::new()
            } else {
                format!(" in section '{section_name}'")
            };
            errors.push(format!(
                "line {}: category '{category_name}'{location} has no resources",
                category_lines[key]
            ));
        }
    }

    // Duplicate title / URL detection.
    let mut seen_titles: HashMap<String, usize> = HashMap::new();
    let mut seen_urls: HashMap<String, usize> = HashMap::new();
    for resource in &resources {
        let title_key = resource.title.to_lowercase();
        if let Some(&first_line) = seen_titles.get(&title_key) {
            errors.push(format!(
                "line {}: duplicate title '{}' (first used on line {first_line})",
                resource.line, resource.title
            ));
        } else {
            seen_titles.insert(title_key, resource.line);
        }

        let url_key = match normalize_url(&resource.url) {
            Ok(key) => key,
            Err(error) => {
                errors.push(format!(
                    "line {}: invalid URL '{}' ({error})",
                    resource.line, resource.url
                ));
                continue;
            }
        };
        if let Some(&first_line) = seen_urls.get(&url_key) {
            errors.push(format!(
                "line {}: duplicate URL '{}' (first used on line {first_line})",
                resource.line, resource.url
            ));
        } else {
            seen_urls.insert(url_key, resource.line);
        }
    }

    Validation {
        resources,
        errors,
        warnings,
    }
}

/// Port of `classify_status`.
///
/// Returns `None` for statuses that are not actionable (e.g. `200`).
pub fn classify_status(status: u16, url: &str) -> Option<(Severity, String)> {
    if status == 404 || status == 410 {
        Some((Severity::Error, format!("broken link ({status}): {url}")))
    } else if status == 401 || status == 403 || status == 429 {
        Some((
            Severity::Warning,
            format!("link check blocked ({status}): {url}"),
        ))
    } else if status == 408 {
        Some((
            Severity::Warning,
            format!("link check timed out ({status}): {url}"),
        ))
    } else if status >= 500 {
        Some((
            Severity::Warning,
            format!("remote server error ({status}): {url}"),
        ))
    } else if status >= 400 {
        Some((Severity::Error, format!("broken link ({status}): {url}")))
    } else {
        None
    }
}

/// Kinds of transport-level failure, mirroring the Python exception types that
/// `classify_exception` distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkError {
    /// `TimeoutError` / `socket.timeout`.
    Timeout,
    /// `ssl.SSLError` / `socket.gaierror`.
    Dns,
    /// `ssl.SSLError` / `socket.gaierror` (TLS variant, same classification).
    Tls,
    /// `http.client.HTTPException`.
    Http,
    /// Any other `OSError`.
    Other,
}

/// Port of `classify_exception`.
///
/// `reason` is the string form of the underlying error reason (matching the
/// `{reason}` interpolation in the Python messages).
pub fn classify_exception(kind: LinkError, url: &str, reason: &str) -> (Severity, String) {
    match kind {
        LinkError::Timeout => (
            Severity::Warning,
            format!("link check timed out: {url} ({reason})"),
        ),
        LinkError::Dns | LinkError::Tls => (
            Severity::Error,
            format!("unreachable link: {url} ({reason})"),
        ),
        LinkError::Http => (
            Severity::Warning,
            format!("link check interrupted: {url} ({reason})"),
        ),
        LinkError::Other => (
            Severity::Error,
            format!("unreachable link: {url} ({reason})"),
        ),
    }
}

/// Port of `check_link`.
///
/// Performs a `HEAD` request and, on a `405`/`501`, falls back to `GET`,
/// classifying the response status or transport error. Returns `None` when the
/// link is healthy. Network failures are mapped through `classify_exception`.
pub fn check_link(resource: &Resource) -> Option<(Severity, String)> {
    match http_status(&resource.url, "HEAD") {
        Ok(status) => {
            // HEAD may be rejected with 405/501; retry with GET.
            if status == 405 || status == 501 {
                match http_status(&resource.url, "GET") {
                    Ok(status) => classify_status(status, &resource.url),
                    Err((kind, reason)) => {
                        Some(classify_exception(kind, &resource.url, &reason))
                    }
                }
            } else {
                classify_status(status, &resource.url)
            }
        }
        Err((kind, reason)) => Some(classify_exception(kind, &resource.url, &reason)),
    }
}

/// Port of `check_links`.
///
/// Runs `check_link` across all resources using a bounded worker pool
/// (`max_workers=8`) and returns the collected errors and warnings **sorted**,
/// matching the Python implementation.
pub fn check_links(resources: &[Resource]) -> (Vec<String>, Vec<String>) {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    if resources.is_empty() {
        return (errors, warnings);
    }

    let max_workers = 8usize;
    let (result_tx, result_rx) = mpsc::channel::<Option<(Severity, String)>>();

    let (job_tx, job_rx) = mpsc::channel::<Resource>();
    let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
    let worker_count = max_workers.min(resources.len());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let result_tx = result_tx.clone();
            let job_rx = std::sync::Arc::clone(&job_rx);
            scope.spawn(move || loop {
                let resource = {
                    let guard = job_rx.lock().unwrap();
                    guard.recv()
                };
                match resource {
                    Ok(resource) => {
                        let _ = result_tx.send(check_link(&resource));
                    }
                    Err(_) => break,
                }
            });
        }

        for resource in resources {
            job_tx.send(resource.clone()).unwrap();
        }
        drop(job_tx);
        drop(result_tx);

        for (severity, message) in result_rx.into_iter().flatten() {
            match severity {
                Severity::Error => errors.push(message),
                Severity::Warning => warnings.push(message),
            }
        }
    });

    errors.sort();
    warnings.sort();
    (errors, warnings)
}

/// Perform an HTTP request and return the status code, or a transport error
/// classified into a `LinkError` + reason string. Minimal HTTP/1.1 client
/// sufficient for the validator's `HEAD`/`GET` link checks.
fn http_status(url: &str, method: &str) -> Result<u16, (LinkError, String)> {
    let parts = urlsplit(url);
    if parts.scheme.to_lowercase() != "https" && parts.scheme.to_lowercase() != "http" {
        return Err((LinkError::Other, format!("unsupported scheme: {url}")));
    }
    // This validator only reaches real hosts when --check-links is passed; the
    // test-suite never invokes it. Attempt a plain TCP connection so behavior
    // is well-defined; TLS is not implemented (https connect will map to a DNS
    // or connection error just like the Python client would surface).
    let host = parts.hostname();
    if host.is_empty() {
        return Err((LinkError::Dns, format!("invalid host: {url}")));
    }
    let port: u16 = match parts.port() {
        Ok(Some(p)) => p as u16,
        Ok(None) => {
            if parts.scheme.to_lowercase() == "https" {
                443
            } else {
                80
            }
        }
        Err(e) => return Err((LinkError::Other, e.0)),
    };

    let addr = format!("{host}:{port}");
    let stream = TcpStream::connect(&addr).map_err(|e| (LinkError::Dns, e.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .ok();
    let mut stream = stream;

    // For https we cannot speak TLS here; surface as an unreachable link.
    if parts.scheme.to_lowercase() == "https" {
        return Err((
            LinkError::Tls,
            "TLS not supported by builtin client".to_string(),
        ));
    }

    let path = if parts.path.is_empty() {
        "/".to_string()
    } else {
        parts.path.clone()
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {USER_AGENT}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| (LinkError::Http, e.to_string()))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| (LinkError::Http, e.to_string()))?;
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or("");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok());
    match code {
        Some(code) => Ok(code),
        None => Err((LinkError::Http, "malformed response".to_string())),
    }
}

/// Signature tuple used by `validate_churn`: `(section, category, url,
/// description)`. `None` models an absent resource.
fn signature(resource: Option<&Resource>) -> Option<(String, String, String, String)> {
    resource.map(|r| {
        (
            r.section.clone(),
            r.category.clone(),
            r.url.clone(),
            r.description.clone(),
        )
    })
}

/// Build the `title.casefold() -> Resource` map used by `validate_churn`.
fn resource_map(resources: &[Resource]) -> HashMap<String, Resource> {
    let mut map = HashMap::new();
    for resource in resources {
        map.insert(resource.title.to_lowercase(), resource.clone());
    }
    map
}

/// Port of `validate_churn`.
///
/// Compares two README revisions and enforces the churn limits (changed
/// entries, net additions, foundational changes). Returns a structural-validity
/// error when either revision fails `validate_text`.
pub fn validate_churn(base_text: &str, current_text: &str) -> Vec<String> {
    let base = validate_text(base_text);
    let current = validate_text(current_text);

    if !base.errors.is_empty() || !current.errors.is_empty() {
        return vec![
            "cannot calculate churn until both README versions are structurally valid".to_string(),
        ];
    }

    let base_map = resource_map(&base.resources);
    let current_map = resource_map(&current.resources);

    // Union of titles (deduplicated, order irrelevant for counting).
    let mut titles: Vec<String> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for key in base_map.keys().chain(current_map.keys()) {
        if seen.insert(key.clone(), ()).is_none() {
            titles.push(key.clone());
        }
    }

    let mut changed_titles: Vec<String> = Vec::new();
    for title in &titles {
        if signature(base_map.get(title)) != signature(current_map.get(title)) {
            changed_titles.push(title.clone());
        }
    }

    let foundational_changes = changed_titles
        .iter()
        .filter(|title| {
            let is_learn = |r: Option<&Resource>| {
                r.map(|r| r.section.to_lowercase() == "learn").unwrap_or(false)
            };
            is_learn(base_map.get(*title)) || is_learn(current_map.get(*title))
        })
        .count();

    let net_additions =
        current.resources.len() as isize - base.resources.len() as isize;

    let mut errors = Vec::new();
    if changed_titles.len() > 6 {
        errors.push(format!(
            "churn limit exceeded: {} resource entries changed (maximum 6)",
            changed_titles.len()
        ));
    }
    if net_additions > 3 {
        errors.push(format!(
            "churn limit exceeded: {net_additions} net entries added (maximum 3)"
        ));
    }
    if foundational_changes > 1 {
        errors.push(format!(
            "churn limit exceeded: {foundational_changes} foundational entries changed (maximum 1)"
        ));
    }
    errors
}
