//! Minimal Chrome DevTools Protocol client.
//!
//! Launches Chrome with a private debugging pipe by default and can also attach over a
//! WebSocket. Only what the navigator and canvas exporter need — not a general CDP binding.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use command_fds::{CommandFdExt, FdMapping};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// Executables `chrome_path` will accept, best first: a managed profile is
/// driven with Chromium's own switches (`--remote-debugging-pipe`,
/// `--user-data-dir`), which no other engine understands.
const CHROME_BINARIES: [&str; 6] = [
    "google-chrome-stable",
    "google-chrome",
    "chromium",
    "chromium-browser",
    "brave-browser",
    "microsoft-edge",
];

/// macOS ships browsers as bundles that are not on `PATH`, and prefers them to
/// anything a package manager dropped in `/usr/local/bin`.
#[cfg(target_os = "macos")]
const CHROME_BUNDLES: [&str; 4] = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

/// Substrings that make a file name a Chromium-family browser. Checked against
/// `$CHROME` and `$BROWSER` so a plain `$BROWSER=firefox` is ignored rather
/// than launched with switches it will show the user as a URL.
const CHROME_MARKERS: [&str; 3] = ["chrom", "brave", "edge"];

/// The browser a managed profile runs in: `$CHROME`, then `$BROWSER`, then the
/// installed Chromium-family binaries. `None` means none is installed, which is
/// a fact to report rather than a path to fail on later.
pub fn chrome_path() -> Option<PathBuf> {
    // One path, spaces and all: a macOS bundle executable has them.
    if let Some(value) = std::env::var_os("CHROME") {
        let path = PathBuf::from(value);
        if is_chrome_family(&path)
            && let Some(found) = locate(&path)
        {
            return Some(found);
        }
    }
    // XDG-shaped instead: a colon-separated list of commands, each of which may
    // carry a `%s` URL placeholder.
    if let Some(value) = std::env::var_os("BROWSER") {
        for entry in std::env::split_paths(&value) {
            let entry = entry.to_string_lossy();
            let Some(command) = entry.split_whitespace().next() else {
                continue;
            };
            let path = Path::new(command);
            if is_chrome_family(path)
                && let Some(found) = locate(path)
            {
                return Some(found);
            }
        }
    }
    #[cfg(target_os = "macos")]
    for bundle in CHROME_BUNDLES {
        if let Some(found) = executable(Path::new(bundle)) {
            return Some(found);
        }
    }
    CHROME_BINARIES
        .into_iter()
        .find_map(|name| locate(Path::new(name)))
}

fn is_chrome_family(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    CHROME_MARKERS.iter().any(|marker| name.contains(marker))
}

/// A path as given if it names an executable file; a bare name off `PATH`.
fn locate(path: &Path) -> Option<PathBuf> {
    if path
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        return executable(path);
    }
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .find_map(|dir| executable(&dir.join(path)))
}

fn executable(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then(|| path.to_path_buf())
}

/// Where Chrome publishes the debugging endpoint of a profile it is running.
/// Written on startup, removed on a clean exit.
const PORT_FILE: &str = "DevToolsActivePort";

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("chrome did not start: {0}")]
    Launch(String),
    #[error("websocket: {0}")]
    Ws(String),
    #[error("{method}: {message}")]
    Protocol { method: String, message: String },
    #[error("evaluation threw: {0}")]
    Exception(String),
    #[error("connection closed")]
    Closed,
    #[error("timed out: {0}")]
    Timeout(&'static str),
}

pub type Result<T> = std::result::Result<T, CdpError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugTransport {
    Pipe,
    WebSocket,
}

/// How a [`Browser`] handle came to exist, which is the difference between a
/// cold start and a warm one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// A Chrome was already running on this profile and was joined.
    Attached,
    /// No Chrome was running on this profile, so one was started.
    Launched,
}

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub chrome: PathBuf,
    pub profile_dir: PathBuf,
    pub headless: bool,
    pub window: (u32, u32),
    pub transport: DebugTransport,
    /// Leave Chrome running when the handle drops.
    ///
    /// The managed profile holds the user's logins, and a run that killed it
    /// on the way out would make the next run cold and the sign-in
    /// pointless (10 §10). A throwaway profile is the opposite: its Chrome
    /// must die with the run, or it outlives the directory it is reading.
    pub keep_alive: bool,
}

impl LaunchOptions {
    /// The managed Chrome: headed, persistent, and reachable again next run.
    ///
    /// Headed because the whole point of a profile that survives is that the
    /// user signs into their accounts in it by hand; a headless default
    /// makes everything behind a login unreachable. The WebSocket transport
    /// is what makes the *next* process able to find this Chrome at all: a
    /// debugging pipe belongs to the process that spawned it, so a pipe
    /// launch on a shared profile can only ever be relaunched, never joined.
    pub fn new(profile_dir: impl Into<PathBuf>) -> Self {
        Self {
            // Empty when no browser is installed; `Browser::launch` names that
            // rather than spawning a path nobody chose.
            chrome: chrome_path().unwrap_or_default(),
            profile_dir: profile_dir.into(),
            headless: false,
            window: (1120, 780),
            transport: DebugTransport::WebSocket,
            keep_alive: true,
        }
    }

    /// A Chrome that belongs to one run: headless, driven over a private
    /// pipe no other process can reach, and killed when the handle drops.
    /// What an eval suite or CI wants, where there is nobody to sign in and
    /// nothing should be left behind.
    pub fn ephemeral(profile_dir: impl Into<PathBuf>) -> Self {
        Self {
            chrome: chrome_path().unwrap_or_default(),
            profile_dir: profile_dir.into(),
            headless: true,
            window: (1120, 780),
            transport: DebugTransport::Pipe,
            keep_alive: false,
        }
    }
}

/// An event pushed by Chrome: `(session id, method, params)`.
pub type Event = (Option<String>, String, Value);

struct Inner {
    next_id: AtomicU64,
    outbound: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>,
    events: broadcast::Sender<Event>,
    calls: AtomicU64,
}

/// A running Chrome plus the browser-level connection.
///
/// Dropping it kills a Chrome this handle launched to own; a `keep_alive`
/// launch and an attach both leave the browser running, because the profile
/// they are driving outlives the run (10 §10).
pub struct Browser {
    inner: Arc<Inner>,
    child: Option<Child>,
}

impl Browser {
    /// The managed Chrome for `options.profile_dir`: the one already running
    /// on that profile when there is one, otherwise a newly launched one.
    ///
    /// A profile directory is single-writer state — a second Chrome on it
    /// either refuses to start or fights the first for the session, and the
    /// user's logins live in it — so a run joins what is there instead of
    /// starting a rival. Chrome publishes the endpoint in [`PORT_FILE`] and
    /// removes it on a clean exit; a file a crash left behind simply fails to
    /// connect, and the launch below rewrites it.
    pub async fn attach_or_launch(options: &LaunchOptions) -> Result<(Self, Start)> {
        // A pipe launch is private to the process that spawned it: there is
        // no endpoint for anyone else to find, so there is nothing to join.
        if options.transport == DebugTransport::WebSocket
            && let Some(endpoint) = read_endpoint(&options.profile_dir.join(PORT_FILE))
            && let Ok(browser) = Self::connect(&endpoint).await
        {
            return Ok((browser, Start::Attached));
        }
        Self::launch(options)
            .await
            .map(|browser| (browser, Start::Launched))
    }

    pub async fn launch(options: &LaunchOptions) -> Result<Self> {
        std::fs::create_dir_all(&options.profile_dir)
            .map_err(|e| CdpError::Launch(e.to_string()))?;
        if options.chrome.as_os_str().is_empty() {
            return Err(CdpError::Launch(
                "no Chromium-family browser found; install one or set $CHROME".to_owned(),
            ));
        }
        let mut command = Command::new(&options.chrome);
        command
            .arg(format!("--user-data-dir={}", options.profile_dir.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-timer-throttling")
            .arg("--disable-renderer-backgrounding")
            .arg(format!(
                "--window-size={},{}",
                options.window.0, options.window.1
            ))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(!options.keep_alive);
        // A Wayland session otherwise gets Chromium through XWayland, where the
        // window arrives as an X11 client with no `app_id` — and `app_id` is
        // what the compositor's rules and the native path address a window by.
        #[cfg(target_os = "linux")]
        command.arg("--ozone-platform-hint=auto");
        if options.headless {
            command.arg("--headless=new");
        }

        match options.transport {
            DebugTransport::Pipe => {
                let (chrome_input, parent_output) =
                    std::io::pipe().map_err(|e| CdpError::Launch(e.to_string()))?;
                let (parent_input, chrome_output) =
                    std::io::pipe().map_err(|e| CdpError::Launch(e.to_string()))?;
                command.arg("--remote-debugging-pipe");
                command
                    .as_std_mut()
                    .fd_mappings(vec![
                        FdMapping {
                            parent_fd: chrome_input.into(),
                            child_fd: 3,
                        },
                        FdMapping {
                            parent_fd: chrome_output.into(),
                            child_fd: 4,
                        },
                    ])
                    .map_err(|e| CdpError::Launch(e.to_string()))?;
                let child = command
                    .spawn()
                    .map_err(|e| CdpError::Launch(e.to_string()))?;
                drop(command);
                let parent_input: OwnedFd = parent_input.into();
                let parent_output: OwnedFd = parent_output.into();
                let mut browser =
                    Self::connect_pipe(File::from(parent_input), File::from(parent_output));
                browser.child = keep(child, options);
                Ok(browser)
            }
            DebugTransport::WebSocket => {
                let port_file = options.profile_dir.join(PORT_FILE);
                let _ = std::fs::remove_file(&port_file);
                command.arg("--remote-debugging-port=0");
                let child = command
                    .spawn()
                    .map_err(|e| CdpError::Launch(e.to_string()))?;
                let endpoint = wait_for_endpoint(&port_file, Duration::from_secs(20)).await?;
                let mut browser = Self::connect(&endpoint).await?;
                browser.child = keep(child, options);
                Ok(browser)
            }
        }
    }

    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (socket, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| CdpError::Ws(e.to_string()))?;
        let (mut sink, mut stream) = socket.split();
        let (inner, mut outbound_rx) = new_inner();

        tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                if sink.send(Message::Text(message.into())).await.is_err() {
                    break;
                }
            }
        });

        let reader = inner.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                route_message(&reader, value).await;
            }
            reader.pending.lock().await.clear();
        });

        Ok(Self { inner, child: None })
    }

    fn connect_pipe(reader: File, mut writer: File) -> Self {
        let (inner, mut outbound_rx) = new_inner();
        tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                if writer.write_all(message.as_bytes()).is_err() || writer.write_all(&[0]).is_err()
                {
                    break;
                }
            }
        });

        let (incoming, mut incoming_rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            let mut frame = Vec::new();
            loop {
                frame.clear();
                match reader.read_until(0, &mut frame) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if frame.last() == Some(&0) {
                    frame.pop();
                }
                if let Ok(value) = serde_json::from_slice::<Value>(&frame)
                    && incoming.send(value).is_err()
                {
                    break;
                }
            }
        });

        let dispatcher = inner.clone();
        tokio::spawn(async move {
            while let Some(value) = incoming_rx.recv().await {
                route_message(&dispatcher, value).await;
            }
            dispatcher.pending.lock().await.clear();
        });

        Self { inner, child: None }
    }

    /// Protocol calls made so far, for measuring how chatty a loop is.
    pub fn calls(&self) -> u64 {
        self.inner.calls.load(Ordering::Relaxed)
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        call(&self.inner, None, method, params).await
    }

    /// Opens a tab the caller owns and attaches a flat session to it.
    pub async fn new_page(&self, url: &str) -> Result<Page> {
        let target = self
            .call("Target.createTarget", json!({ "url": "about:blank" }))
            .await?;
        let target_id = target["targetId"].as_str().unwrap_or_default().to_owned();
        let page = attach_page(self.inner.clone(), target_id).await?;
        if url != "about:blank" {
            page.navigate(url).await?;
        }
        Ok(page)
    }

    /// Close one target, by the id the caller got when it opened it.
    ///
    /// The bot works only in tabs it opened (10 §10), so this is never
    /// reached with an id the caller did not create. An id Chrome no longer
    /// knows — a tab the user closed, or a browser that restarted since —
    /// answers with a protocol error, which is a fact, not a failure.
    pub async fn close_target(&self, target_id: &str) -> Result<()> {
        self.call("Target.closeTarget", json!({ "targetId": target_id }))
            .await
            .map(drop)
    }

    pub async fn close(mut self) {
        let _ = self.call("Browser.close", json!({})).await;
        if let Some(mut child) = self.child.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        }
    }
}

/// The child handle a launch keeps, if any.
///
/// A `keep_alive` Chrome must outlive this process, so its handle is dropped
/// rather than stored: `kill_on_drop` is already off, and letting tokio's
/// orphan reaper take the handle means a browser the user later quits does
/// not sit in the process table until we exit.
fn keep(child: Child, options: &LaunchOptions) -> Option<Child> {
    (!options.keep_alive).then_some(child)
}

fn new_inner() -> (Arc<Inner>, mpsc::UnboundedReceiver<String>) {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (events, _) = broadcast::channel(1024);
    (
        Arc::new(Inner {
            next_id: AtomicU64::new(1),
            outbound,
            pending: Mutex::new(HashMap::new()),
            events,
            calls: AtomicU64::new(0),
        }),
        outbound_rx,
    )
}

async fn route_message(inner: &Arc<Inner>, value: Value) {
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        if let Some(reply) = inner.pending.lock().await.remove(&id) {
            let result = match value.get("error") {
                Some(error) => Err(error["message"].as_str().unwrap_or("unknown").to_owned()),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = reply.send(result);
        }
    } else if let Some(method) = value.get("method").and_then(Value::as_str) {
        let session = value
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let _ = inner.events.send((session, method.to_owned(), params));
    }
}

async fn call(
    inner: &Arc<Inner>,
    session: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value> {
    let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
    inner.calls.fetch_add(1, Ordering::Relaxed);
    let mut message = json!({ "id": id, "method": method, "params": params });
    if let Some(session) = session {
        message["sessionId"] = json!(session);
    }
    let (reply, wait) = oneshot::channel();
    inner.pending.lock().await.insert(id, reply);
    inner
        .outbound
        .send(message.to_string())
        .map_err(|_| CdpError::Closed)?;
    match tokio::time::timeout(Duration::from_secs(30), wait).await {
        Err(_) => Err(CdpError::Timeout("protocol reply")),
        Ok(Err(_)) => Err(CdpError::Closed),
        Ok(Ok(Err(message))) => Err(CdpError::Protocol {
            method: method.to_owned(),
            message,
        }),
        Ok(Ok(Ok(value))) => Ok(value),
    }
}

async fn attach_page(inner: Arc<Inner>, target_id: String) -> Result<Page> {
    let attached = call(
        &inner,
        None,
        "Target.attachToTarget",
        json!({ "targetId": target_id, "flatten": true }),
    )
    .await?;
    let session = attached["sessionId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let page = Page {
        inner,
        session,
        target_id,
        frame_sessions: Arc::new(Mutex::new(HashMap::new())),
        context_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    page.call("Page.enable", json!({})).await?;
    page.call("Runtime.enable", json!({})).await?;
    page.call(
        "Emulation.setFocusEmulationEnabled",
        json!({ "enabled": true }),
    )
    .await?;
    Ok(page)
}

async fn wait_for_endpoint(port_file: &Path, limit: Duration) -> Result<String> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if let Some(endpoint) = read_endpoint(port_file) {
            return Ok(endpoint);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(CdpError::Launch("DevToolsActivePort never appeared".into()))
}

/// The endpoint a running Chrome published for its profile, if one is there.
fn read_endpoint(port_file: &Path) -> Option<String> {
    endpoint_in(&std::fs::read_to_string(port_file).ok()?)
}

/// `DevToolsActivePort` is two lines: the port, then the browser's WebSocket
/// path. Both are checked because this file is read while Chrome is still
/// writing it — a half-written file is a browser that is not up yet, not an
/// endpoint to connect to.
fn endpoint_in(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    (port != 0 && path.starts_with('/')).then(|| format!("ws://127.0.0.1:{port}{path}"))
}

#[derive(Debug, Clone)]
pub struct ChildFrame {
    pub id: String,
    pub offset_x: f64,
    pub offset_y: f64,
}
/// One tab, addressed through its flat session.
#[derive(Clone)]
pub struct Page {
    inner: Arc<Inner>,
    session: String,
    target_id: String,
    frame_sessions: Arc<Mutex<HashMap<String, String>>>,
    context_sessions: Arc<Mutex<HashMap<u64, String>>>,
}

fn collect_child_frame_ids(tree: &Value, ids: &mut Vec<String>) {
    if let Some(children) = tree["childFrames"].as_array() {
        for child in children {
            if let Some(id) = child["frame"]["id"].as_str() {
                ids.push(id.to_owned());
            }
            collect_child_frame_ids(child, ids);
        }
    }
}

impl Page {
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        call(&self.inner, Some(&self.session), method, params).await
    }

    async fn call_session(&self, session: &str, method: &str, params: Value) -> Result<Value> {
        call(&self.inner, Some(session), method, params).await
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub async fn activate(&self) -> Result<()> {
        call(
            &self.inner,
            None,
            "Target.activateTarget",
            json!({ "targetId": self.target_id }),
        )
        .await
        .map(drop)
    }

    /// Attaches only to a page whose opener is this owned target.
    pub async fn popup(&self, wait: Duration) -> Result<Option<Page>> {
        let deadline = Instant::now() + wait;
        loop {
            let targets = call(&self.inner, None, "Target.getTargets", json!({})).await?;
            if let Some(info) = targets["targetInfos"].as_array().and_then(|targets| {
                targets.iter().find(|info| {
                    info["type"] == "page"
                        && info["openerId"].as_str() == Some(self.target_id.as_str())
                })
            }) {
                let target_id = info["targetId"].as_str().unwrap_or_default().to_owned();
                return attach_page(self.inner.clone(), target_id).await.map(Some);
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.call("Page.navigate", json!({ "url": url })).await?;
        self.frame_sessions.lock().await.clear();
        self.context_sessions.lock().await.clear();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(state) = self.evaluate("document.readyState").await
                && state == "complete"
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(CdpError::Timeout("page load"))
    }

    pub async fn set_viewport(&self, width: u32, height: u32, scale: f64) -> Result<()> {
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": width, "height": height, "deviceScaleFactor": scale, "mobile": false }),
        )
        .await
        .map(drop)
    }

    /// Evaluates an expression and returns its JSON value.
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        self.evaluate_with(expression, false).await
    }

    pub async fn evaluate_async(&self, expression: &str) -> Result<Value> {
        self.evaluate_with(expression, true).await
    }

    async fn evaluate_with(&self, expression: &str, await_promise: bool) -> Result<Value> {
        let response = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": await_promise }),
            )
            .await?;
        if let Some(details) = response.get("exceptionDetails") {
            return Err(CdpError::Exception(
                details["text"].as_str().unwrap_or("exception").to_owned(),
            ));
        }
        Ok(response["result"]
            .get("value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn create_isolated_world(&self, name: &str) -> Result<u64> {
        let tree = self.call("Page.getFrameTree", json!({})).await?;
        let frame_id =
            tree["frameTree"]["frame"]["id"]
                .as_str()
                .ok_or_else(|| CdpError::Protocol {
                    method: "Page.createIsolatedWorld".into(),
                    message: "page has no main frame id".into(),
                })?;
        self.create_isolated_world_for_frame(frame_id, name).await
    }

    async fn ensure_frame_session(&self, frame_id: &str) -> Result<Option<String>> {
        if let Some(session) = self.frame_sessions.lock().await.get(frame_id).cloned() {
            return Ok(Some(session));
        }
        let targets = call(&self.inner, None, "Target.getTargets", json!({})).await?;
        let target = targets["targetInfos"].as_array().and_then(|targets| {
            targets.iter().find(|target| {
                target["type"] == "iframe" && target["targetId"].as_str() == Some(frame_id)
            })
        });
        let Some(target_id) = target.and_then(|target| target["targetId"].as_str()) else {
            return Ok(None);
        };
        let attached = call(
            &self.inner,
            None,
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
        )
        .await?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or_else(|| CdpError::Protocol {
                method: "Target.attachToTarget".into(),
                message: "Chrome returned no OOPIF session".into(),
            })?
            .to_owned();
        self.call_session(&session, "Page.enable", json!({}))
            .await?;
        self.call_session(&session, "Runtime.enable", json!({}))
            .await?;
        self.frame_sessions
            .lock()
            .await
            .insert(frame_id.to_owned(), session.clone());
        Ok(Some(session))
    }

    pub async fn create_isolated_world_for_frame(&self, frame_id: &str, name: &str) -> Result<u64> {
        let session = self
            .ensure_frame_session(frame_id)
            .await?
            .unwrap_or_else(|| self.session.clone());
        let world = self
            .call_session(
                &session,
                "Page.createIsolatedWorld",
                json!({
                    "frameId": frame_id,
                    "worldName": name,
                    "grantUniveralAccess": true,
                }),
            )
            .await?;
        let context_id =
            world["executionContextId"]
                .as_u64()
                .ok_or_else(|| CdpError::Protocol {
                    method: "Page.createIsolatedWorld".into(),
                    message: "Chrome returned no execution context".into(),
                })?;
        self.context_sessions
            .lock()
            .await
            .insert(context_id, session);
        Ok(context_id)
    }

    pub async fn child_frames(&self) -> Result<Vec<ChildFrame>> {
        let tree = self.call("Page.getFrameTree", json!({})).await?;
        let mut ids = Vec::new();
        collect_child_frame_ids(&tree["frameTree"], &mut ids);
        let targets = call(&self.inner, None, "Target.getTargets", json!({})).await?;
        for target in targets["targetInfos"].as_array().into_iter().flatten() {
            let direct_child = target["type"] == "iframe"
                && target["parentId"].as_str() == Some(self.target_id.as_str());
            if let Some(id) = target["targetId"].as_str()
                && direct_child
                && !ids.iter().any(|candidate| candidate == id)
            {
                ids.push(id.to_owned());
            }
        }
        let mut frames = Vec::with_capacity(ids.len());
        for id in ids {
            self.ensure_frame_session(&id).await?;
            let owner = self
                .call("DOM.getFrameOwner", json!({ "frameId": id }))
                .await?;
            let mut params = json!({});
            if let Some(backend_node_id) = owner["backendNodeId"].as_u64() {
                params["backendNodeId"] = json!(backend_node_id);
            } else if let Some(node_id) = owner["nodeId"].as_u64() {
                params["nodeId"] = json!(node_id);
            } else {
                continue;
            }
            // Chrome lists hidden iframes in the frame tree, but their owner
            // element has no layout box. They cannot contribute visible text
            // or actionable controls, so omit them instead of making the
            // containing page unreadable.
            let model = match self.call("DOM.getBoxModel", params).await {
                Ok(model) => model,
                Err(CdpError::Protocol { method, message })
                    if method == "DOM.getBoxModel"
                        && message.contains("Could not compute box model") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let quad = model["model"]["content"].as_array();
            let offset_x = quad
                .and_then(|quad| quad.first())
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let offset_y = quad
                .and_then(|quad| quad.get(1))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            frames.push(ChildFrame {
                id,
                offset_x,
                offset_y,
            });
        }
        Ok(frames)
    }

    pub async fn evaluate_in_context(
        &self,
        context_id: u64,
        expression: &str,
        await_promise: bool,
    ) -> Result<Value> {
        let session = self
            .context_sessions
            .lock()
            .await
            .get(&context_id)
            .cloned()
            .unwrap_or_else(|| self.session.clone());
        let response = self
            .call_session(
                &session,
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": await_promise,
                    "contextId": context_id,
                }),
            )
            .await?;
        if let Some(details) = response.get("exceptionDetails") {
            return Err(CdpError::Exception(
                details["text"].as_str().unwrap_or("exception").to_owned(),
            ));
        }
        Ok(response["result"]
            .get("value")
            .cloned()
            .unwrap_or(Value::Null))
    }
    pub async fn click(&self, x: f64, y: f64) -> Result<()> {
        for kind in ["mousePressed", "mouseReleased"] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn wheel(&self, x: f64, y: f64, delta_y: f64) -> Result<()> {
        self.call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": delta_y }),
        )
        .await
        .map(drop)
    }

    /// Replaces the focused field's contents: select-all, then native text insertion.
    ///
    /// The accelerator has to be the one this platform's users press: `Meta+A`
    /// on macOS, `Ctrl+A` everywhere else. Two things read it. Blink's own
    /// binding table turns the event into SelectAll off `windowsVirtualKeyCode`
    /// — drop that field and nothing is selected, so `insertText` appends to
    /// the old value instead of replacing it. A page that implements its own
    /// select-all reads `ctrlKey`/`metaKey`, and every editor binds the
    /// platform's own modifier, so the wrong one silently misses its handler.
    /// `commands` is the editing-command list AppKit delivers alongside the
    /// keystroke; it belongs to the macOS path only.
    pub async fn replace_text(&self, text: &str) -> Result<()> {
        const META: u32 = 4;
        const CTRL: u32 = 2;
        const SELECT_ALL: u32 = if cfg!(target_os = "macos") {
            META
        } else {
            CTRL
        };
        const KEY_A: u32 = 65;

        let mut key_down = json!({
            "type": "keyDown",
            "key": "a",
            "code": "KeyA",
            "modifiers": SELECT_ALL,
            "windowsVirtualKeyCode": KEY_A,
            "nativeVirtualKeyCode": KEY_A,
        });
        if cfg!(target_os = "macos") {
            key_down["commands"] = json!(["selectAll"]);
        }
        self.call("Input.dispatchKeyEvent", key_down).await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": "a",
                "code": "KeyA",
                "modifiers": SELECT_ALL,
                "windowsVirtualKeyCode": KEY_A,
                "nativeVirtualKeyCode": KEY_A,
            }),
        )
        .await?;
        self.call("Input.insertText", json!({ "text": text }))
            .await
            .map(drop)
    }

    /// Inserts multiline content without a bare Enter that could submit a composer.
    pub async fn replace_text_multiline(&self, text: &str) -> Result<()> {
        // Shift, not Meta: `Shift+Enter` is the newline every composer agrees
        // on, and it means the same thing on every platform.
        const SHIFT: u32 = 8;
        let mut lines = text.split('\n');
        self.replace_text(lines.next().unwrap_or_default()).await?;
        for line in lines {
            self.call(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": "Enter",
                    "code": "Enter",
                    "modifiers": SHIFT,
                    "windowsVirtualKeyCode": 13,
                    "nativeVirtualKeyCode": 13,
                    "text": "\r",
                    "unmodifiedText": "\r",
                }),
            )
            .await?;
            self.call(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyUp",
                    "key": "Enter",
                    "code": "Enter",
                    "modifiers": SHIFT,
                    "windowsVirtualKeyCode": 13,
                    "nativeVirtualKeyCode": 13,
                }),
            )
            .await?;
            self.call("Input.insertText", json!({ "text": line }))
                .await?;
        }
        Ok(())
    }

    pub async fn press_key(&self, key: &str) -> Result<()> {
        let (code, virtual_key) = match key {
            "Enter" => ("Enter", 13),
            "Escape" => ("Escape", 27),
            "Tab" => ("Tab", 9),
            " " => ("Space", 32),
            "Backspace" => ("Backspace", 8),
            "ArrowLeft" => ("ArrowLeft", 37),
            "ArrowUp" => ("ArrowUp", 38),
            "ArrowRight" => ("ArrowRight", 39),
            "ArrowDown" => ("ArrowDown", 40),
            other => (other, 0),
        };
        for kind in ["keyDown", "keyUp"] {
            self.call(
                "Input.dispatchKeyEvent",
                json!({
                    "type": kind,
                    "key": key,
                    "code": code,
                    "windowsVirtualKeyCode": virtual_key,
                    "nativeVirtualKeyCode": virtual_key,
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Sets files on an observed input node without exposing an arbitrary selector.
    pub async fn set_file_input_files(
        &self,
        context_id: u64,
        node: u64,
        files: &[PathBuf],
    ) -> Result<()> {
        let session = self
            .context_sessions
            .lock()
            .await
            .get(&context_id)
            .cloned()
            .unwrap_or_else(|| self.session.clone());
        let resolved = self
            .call_session(
                &session,
                "Runtime.evaluate",
                json!({
                    "expression": format!("window.__jevFast?.nodes.get({node})"),
                    "returnByValue": false,
                    "contextId": context_id,
                }),
            )
            .await?;
        let object_id =
            resolved["result"]["objectId"]
                .as_str()
                .ok_or_else(|| CdpError::Protocol {
                    method: "DOM.setFileInputFiles".into(),
                    message: "observed file input is no longer available".into(),
                })?;
        let described = self
            .call_session(
                &session,
                "DOM.describeNode",
                json!({ "objectId": object_id }),
            )
            .await?;
        let backend_node_id =
            described["node"]["backendNodeId"]
                .as_u64()
                .ok_or_else(|| CdpError::Protocol {
                    method: "DOM.setFileInputFiles".into(),
                    message: "file input has no backend node id".into(),
                })?;
        self.call_session(
            &session,
            "DOM.setFileInputFiles",
            json!({
                "files": files.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
                "backendNodeId": backend_node_id,
            }),
        )
        .await
        .map(drop)
    }

    /// PNG bytes of the page, or of `clip` = (x, y, width, height) in CSS pixels.
    pub async fn screenshot_png(&self, clip: Option<(f64, f64, f64, f64)>) -> Result<Vec<u8>> {
        use base64::Engine;
        let mut params = json!({ "format": "png", "captureBeyondViewport": true });
        if let Some((x, y, width, height)) = clip {
            params["clip"] =
                json!({ "x": x, "y": y, "width": width, "height": height, "scale": 1 });
        }
        let response = self.call("Page.captureScreenshot", params).await?;
        base64::engine::general_purpose::STANDARD
            .decode(response["data"].as_str().unwrap_or_default())
            .map_err(|e| CdpError::Ws(e.to_string()))
    }

    pub async fn close(self) -> Result<()> {
        call(
            &self.inner,
            None,
            "Target.closeTarget",
            json!({ "targetId": self.target_id }),
        )
        .await
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    /// The default used to be headless on a directory the caller threw away,
    /// which put everything behind a login out of reach (16 §0, B4). Headed,
    /// kept alive, and over the transport a *later* process can find again
    /// are the three halves of "Stark's Chrome" — miss any one and the
    /// user's sign-in does not survive to the next run.
    #[test]
    fn the_default_chrome_is_headed_persistent_and_joinable() {
        let options = LaunchOptions::new("/tmp/starkbot-neo/chrome");
        assert!(!options.headless);
        assert!(options.keep_alive);
        assert_eq!(options.transport, DebugTransport::WebSocket);
    }

    /// The opt-in for eval and CI: nobody to sign in, nothing left behind,
    /// and no endpoint on the machine for anything else to drive.
    #[test]
    fn an_ephemeral_chrome_leaves_nothing_behind() {
        let options = LaunchOptions::ephemeral("/tmp/throwaway");
        assert!(options.headless);
        assert!(!options.keep_alive);
        assert_eq!(options.transport, DebugTransport::Pipe);
    }

    /// This file is read while Chrome is still writing it, and a run that
    /// connected to `ws://127.0.0.1:` plus half a path would report a
    /// browser failure for a browser that was merely still starting.
    #[test]
    fn only_a_complete_port_file_is_an_endpoint() {
        assert_eq!(
            endpoint_in("51234\n/devtools/browser/abc\n").as_deref(),
            Some("ws://127.0.0.1:51234/devtools/browser/abc")
        );
        assert_eq!(endpoint_in("51234"), None, "the path line is missing");
        assert_eq!(endpoint_in("51234\ndevtools\n"), None, "not a path");
        assert_eq!(endpoint_in("\n/devtools/browser/abc\n"), None, "no port");
        assert_eq!(endpoint_in(""), None);
    }

    /// A `DevToolsActivePort` left behind by a crashed Chrome must not stop
    /// the next run: nothing answers on that port, so the attach fails and
    /// the launch happens. Proven by the launch being the thing that fails —
    /// the binary does not exist — rather than the attach.
    #[tokio::test]
    async fn a_stale_endpoint_falls_through_to_a_launch() {
        let profile = tempfile::tempdir().expect("a temporary profile");
        // A port nothing is listening on: bound to learn the number, then
        // dropped. Asking the OS beats picking one and hoping.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = closed.local_addr().expect("the bound address").port();
        drop(closed);
        std::fs::write(
            profile.path().join(PORT_FILE),
            format!("{port}\n/devtools/browser/stale\n"),
        )
        .expect("the port file is written");

        let mut options = LaunchOptions::new(profile.path());
        options.chrome = PathBuf::from("/nonexistent/Google Chrome");
        match Browser::attach_or_launch(&options).await {
            Err(CdpError::Launch(_)) => (),
            Err(other) => panic!("{other}"),
            Ok(_) => panic!("nothing was listening, and there is no Chrome to launch"),
        }
    }
}
