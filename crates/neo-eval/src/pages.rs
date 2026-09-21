//! The review set's own web, and how a page's state is read back (16 §6.2).
//!
//! The navigation review set has to be deterministic, so its pages are the
//! repo's own fixtures under `spikes/fixtures`, served from this process over
//! loopback. Nothing here reaches the network: a case that depended on
//! somebody else's site would measure their uptime, not the navigator.
//!
//! # Why a server and not `file://`
//!
//! Two reasons, both load-bearing. The deterministic layer refuses any
//! destination that is not `http(s)` (`jev-nav/src/gate.rs`), so every link a
//! `file://` fixture offered would be refused as "not a web page" — which is
//! one case's expected outcome and would silently break the other fourteen.
//! And an origin is what makes the upload gate, the cross-origin frame and
//! `localStorage` mean anything: `localhost` and `127.0.0.1` are the same
//! server here and two different origins, which is how a no-network fixture
//! still produces a real out-of-process frame.
//!
//! # Why the pages record into `localStorage`
//!
//! A probe must score the page, and it cannot attach to the tab the navigator
//! drove: `neo-cdp` hands out a [`neo_cdp::Page`] only for a tab it opened
//! itself. So each fixture mirrors what happened to it into
//! `localStorage["neo-eval"]`, which belongs to the *profile*, and the probe
//! reads that back from its own tab on the same origin — after the run, even
//! if the tab is gone. The record is written by the page's own event
//! handlers, so it says what the page did, not what the model said it did.

use std::path::{Path, PathBuf};

use neo_cdp::{Browser, LaunchOptions, Page};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::OnceCell;

use crate::probe::ProbeError;

/// The fixture server's port.
///
/// Fixed, not ephemeral: a case's user message names its page, and the review
/// set is a *fixed* corpus (B7) whose messages are the same string every run.
/// A port picked at random would have to be substituted into every message,
/// which makes the corpus depend on the harness's startup order.
pub const PORT: u16 = 8787;

/// The origin every review-set case is driven against.
pub const HOST: &str = "127.0.0.1";

/// The second origin, for the cross-origin frame case. The same server
/// answers it; only the host name differs, which is all an origin is.
pub const ALT_HOST: &str = "localhost";

/// The read-only page the harness opens to reach an origin's record.
const STATE_PAGE: &str = "nav-state.html";

/// Where a fixture writes what happened to it.
const STATE_KEY: &str = "neo-eval";

/// The login fixture's session flag: set by a real sign-in in the page, or by
/// the harness standing in for the human when a run hands over.
const SESSION_KEY: &str = "neo-eval-session";

/// The URL of one fixture page on the review set's origin.
#[must_use]
pub fn url(page: &str) -> String {
    format!("http://{HOST}:{PORT}/{page}")
}

/// The page a probe opens to read an origin's record.
#[must_use]
pub fn state_url() -> String {
    url(STATE_PAGE)
}

/// Chrome's managed profile for a data directory.
///
/// The same directory `neo_agent`'s browser runs use
/// (`agent/tools.rs`: `data_dir.join("chrome")`), because the record a probe
/// reads was written by the profile the run drove. It is a string there and
/// not public, so this is the one place the name is repeated.
#[must_use]
pub fn profile(data_dir: &Path) -> PathBuf {
    data_dir.join("chrome")
}

/// The repo's fixture directory.
///
/// Compiled-in first — an eval is a developer tool run from a checkout — with
/// a working-directory fallback so a binary run from the repo root also finds
/// them.
fn root() -> Result<PathBuf, ProbeError> {
    let compiled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spikes/fixtures");
    if compiled.join(STATE_PAGE).is_file() {
        return Ok(compiled);
    }
    let local = PathBuf::from("spikes/fixtures");
    if local.join(STATE_PAGE).is_file() {
        return Ok(local);
    }
    Err(ProbeError::Fixtures(format!(
        "the review set's pages are not where they should be: {}",
        compiled.display()
    )))
}

/// The server, started at most once per process.
static SERVER: OnceCell<Result<(), String>> = OnceCell::const_new();

/// Start the fixture server if it is not already up.
///
/// Idempotent and shared: the suite runs one case at a time, but every
/// browser case needs the same server, and re-binding the port per case would
/// race Chrome's still-open keep-alive connections.
pub async fn serve() -> Result<(), ProbeError> {
    SERVER
        .get_or_init(|| async {
            match start().await {
                Ok(()) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
        .await
        .clone()
        .map_err(ProbeError::Fixtures)
}

/// Bind the port and serve the fixture directory for the rest of the process.
///
/// The server gets a thread and a runtime of its own rather than
/// `tokio::spawn` on the caller's, because it is started once and outlives
/// whoever started it: the first case's fixture step brings it up, and the
/// remaining cases — and, in a test binary, the remaining tests — must still
/// find it there. Spawned on the caller's runtime it dies whenever that
/// runtime does, leaving the `OnceCell` saying "started" and Chrome getting
/// connection refused, which reads as a page that would not load.
async fn start() -> Result<(), ProbeError> {
    let root = root()?;
    let (ready, bound) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("neo-eval fixtures".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                // Both loopback families, because `localhost` resolves to
                // either and the cross-origin case needs that name to answer.
                match TcpListener::bind((HOST, PORT)).await {
                    Ok(listener) => {
                        if let Ok(v6) = TcpListener::bind(("::1", PORT)).await {
                            listen(v6, root.clone());
                        }
                        listen(listener, root);
                        let _ = ready.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = ready.send(Err(format!("port {PORT} is taken: {error}")));
                        return;
                    }
                }
                // The accept loops live on this runtime, so it has to stay.
                std::future::pending::<()>().await;
            });
        })
        .map_err(|error| ProbeError::Fixtures(error.to_string()))?;
    match bound.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(ProbeError::Fixtures(error)),
        Err(_) => Err(ProbeError::Fixtures(
            "the fixture server stopped before it was listening".to_owned(),
        )),
    }
}

fn listen(listener: TcpListener, root: PathBuf) {
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let root = root.clone();
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let Ok(size) = socket.read(&mut request).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&request[..size]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|path| path.split('?').next())
                    .unwrap_or("/");
                let (status, kind, body) = answer(&root, path).await;
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
                     Cache-Control: no-store\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
}

/// One request, answered from the fixture directory.
///
/// Only a bare file name is served, and only the two extensions the fixtures
/// use: the request path comes from a page the navigator is driving, so it is
/// untrusted input reaching the file system.
async fn answer(root: &Path, path: &str) -> (&'static str, &'static str, Vec<u8>) {
    let name = path.trim_start_matches('/');
    let legible = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && !name.contains("..");
    let kind = match name.rsplit_once('.') {
        Some((_, "html")) => "text/html; charset=utf-8",
        Some((_, "txt")) => "text/plain; charset=utf-8",
        _ => "",
    };
    if !legible || kind.is_empty() {
        return (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not a fixture".to_vec(),
        );
    }
    match tokio::fs::read(root.join(name)).await {
        Ok(body) => ("200 OK", kind, body),
        Err(_) => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"no such fixture".to_vec(),
        ),
    }
}

/// A tab of the harness's own, on the managed profile, for reading or writing
/// an origin's record.
///
/// It attaches to the Chrome the run is using when there is one — the same
/// `attach_or_launch` the navigator takes — so this neither steals the
/// profile nor starts a rival browser on it.
///
/// `headless` is the eval and CI opt-in (16 §0, B4), and it is set here for a
/// reason worth stating: the flag only decides how a Chrome is *launched*, so
/// attaching to a run's headed browser is unaffected, while the harness's own
/// first touch of a fresh profile — the fixture reset, which happens before
/// any run — starts that profile's Chrome without a window. Every case in the
/// suite then attaches to it, so a suite run against a throwaway
/// `--data-dir` neither takes the screen nor opens a window, without the
/// agent's browse tool needing an eval-only flag.
async fn with_page<T>(
    data_dir: &Path,
    url: &str,
    body: impl AsyncFnOnce(&Page) -> Result<T, ProbeError>,
) -> Result<T, ProbeError> {
    let mut options = LaunchOptions::new(profile(data_dir));
    options.headless = true;
    let (browser, _start) = Browser::attach_or_launch(&options)
        .await
        .map_err(|error| ProbeError::Browser(error.to_string()))?;
    let page = browser
        .new_page(url)
        .await
        .map_err(|error| ProbeError::Browser(error.to_string()))?;
    let outcome = body(&page).await;
    // The tab is the harness's, so it is closed either way; the browser is
    // left running, because the profile — and its record — outlive the run.
    let _ = page.close().await;
    outcome
}

/// What an origin's fixtures have recorded, as the probe's arguments.
pub async fn read_state(data_dir: &Path, url: &str) -> Result<Value, ProbeError> {
    with_page(data_dir, url, async |page| {
        let raw = page
            .evaluate(&format!("localStorage.getItem({})", json!(STATE_KEY)))
            .await
            .map_err(|error| ProbeError::Browser(error.to_string()))?;
        let session = page
            .evaluate(&format!("localStorage.getItem({})", json!(SESSION_KEY)))
            .await
            .map_err(|error| ProbeError::Browser(error.to_string()))?;
        Ok(json!({
            "url": url,
            // The record itself, parsed, so assertions read
            // `state.request.name` rather than a string of JSON.
            "state": parse_state(raw.as_str()),
            "session": session.as_str() == Some("open"),
        }))
    })
    .await
}

/// A recorded state, or an empty object when a page never recorded anything.
///
/// An unparseable record is reported as the raw string rather than as "no
/// state": a fixture that wrote something malformed is a fixture bug, and
/// hiding it behind `{}` would read as an agent failure.
fn parse_state(raw: Option<&str>) -> Value {
    match raw {
        None => json!({}),
        Some(text) => serde_json::from_str(text).unwrap_or_else(|_| json!({ "raw": text })),
    }
}

/// Clear an origin's record, so a case starts from a known state.
pub async fn reset_state(data_dir: &Path, url: &str) -> Result<Value, ProbeError> {
    with_page(data_dir, url, async |page| {
        page.evaluate(&format!(
            "(() => {{ localStorage.removeItem({}); localStorage.removeItem({}); \
             return localStorage.getItem({}) === null; }})()",
            json!(STATE_KEY),
            json!(SESSION_KEY),
            json!(STATE_KEY)
        ))
        .await
        .map_err(|error| ProbeError::Browser(error.to_string()))
        .map(|cleared| json!({ "url": url, "cleared": cleared }))
    })
    .await
}

/// Do the part only a person can do: open the fixture's session.
///
/// This is the harness standing in for the human a run handed over to. It
/// writes the same flag the fixture's own sign-in button writes, from a
/// different tab, which is what makes the paused run resume against a page
/// that genuinely changed rather than one that was told it changed.
pub async fn open_session(data_dir: &Path, url: &str) -> Result<(), ProbeError> {
    with_page(data_dir, url, async |page| {
        page.evaluate(&format!(
            "localStorage.setItem({}, 'open')",
            json!(SESSION_KEY)
        ))
        .await
        .map_err(|error| ProbeError::Browser(error.to_string()))
        .map(drop)
    })
    .await
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    /// Every page the review set drives has to be in the checkout, or the
    /// suite measures a missing file. This is the one test that fails when a
    /// fixture is renamed without its case.
    #[test]
    fn every_review_set_page_exists() {
        let root = root().expect("the fixture directory");
        for page in [
            STATE_PAGE,
            "nav-form.html",
            "nav-crossorigin.html",
            "nav-frame.html",
            "nav-shadow.html",
            "nav-upload.html",
            "nav-popup.html",
            "nav-popup-target.html",
            "nav-scroll.html",
            "nav-select.html",
            "nav-keys.html",
            "nav-autocomplete.html",
            "nav-search.html",
            "nav-animation.html",
            "nav-pay.html",
            "nav-login.html",
            "nav-nonweb.html",
            "nav-fact.html",
            "upload.txt",
        ] {
            assert!(
                root.join(page).is_file(),
                "the review set drives {page}, which is not in spikes/fixtures"
            );
        }
    }

    /// The request path reaches the file system from a page the navigator is
    /// driving, so traversal and anything that is not a fixture are refused.
    #[tokio::test]
    async fn only_fixture_files_are_served() {
        let root = root().expect("the fixture directory");
        let (status, _, _) = answer(&root, "/nav-form.html").await;
        assert_eq!(status, "200 OK");

        for hostile in [
            "/../../Cargo.toml",
            "/..%2fCargo.toml",
            "/etc/passwd",
            "/",
            "/nav-form.html.bak",
        ] {
            let (status, _, _) = answer(&root, hostile).await;
            assert_eq!(status, "404 Not Found", "{hostile} must not be served");
        }
    }

    /// A page that recorded nothing is an empty record, and a page that
    /// recorded something malformed says so instead of looking empty.
    #[test]
    fn a_record_is_parsed_or_reported_verbatim() {
        assert_eq!(parse_state(None), json!({}));
        assert_eq!(
            parse_state(Some(r#"{"request":{"name":"Ada"}}"#))["request"]["name"],
            json!("Ada")
        );
        assert_eq!(parse_state(Some("not json"))["raw"], json!("not json"));
    }

    /// The probe reads the record out of the profile the run wrote it in.
    #[test]
    fn the_profile_is_the_one_the_navigator_uses() {
        assert_eq!(
            profile(Path::new("/tmp/data")),
            PathBuf::from("/tmp/data/chrome")
        );
    }
}
