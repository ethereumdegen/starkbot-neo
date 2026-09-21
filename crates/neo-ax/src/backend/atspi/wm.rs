//! Enumerating windows and moving focus.
//!
//! There is no `NSWorkspace` and no portable Wayland protocol for "which
//! windows exist and which one is active" — `wlr-foreign-toplevel` is close
//! but carries no pid, and Hyprland does not implement it. So this is a
//! trait with one implementation (17 §3.2), and a session whose compositor
//! has none is told so by name ([`crate::AxError::NoWindowManager`]) rather
//! than silently reported as "the app has no focused window".
//!
//! The Hyprland implementation speaks its IPC socket directly over a
//! `UnixStream`. The `hyprland` crate is at 0.4.0-beta and would pin the
//! whole workspace to one compositor's release cadence for three requests.
//!
//! **Measured, and it contradicts what 17 §3.2 was written from**: on
//! Hyprland 0.56.2 the documented `dispatch focuswindow pid:N` is gone. The
//! socket wraps the payload in `hl.dispatch(<payload>)` and evaluates it as
//! Lua, so the old string form is a *syntax error*:
//!
//! ```text
//! error: [string "return hl.dispatch(focuswindow pid:279067)"]:1:
//!        ')' expected near 'pid'
//! ```
//!
//! The form that works is `hl.dsp.focus({ window = "pid:N" })`. Both are
//! tried, newest first, because Hyprland releases older than 0.56 accept
//! only the old one and a reply of `ok` is unambiguous.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::error::AxError;

/// One top-level window, as the compositor sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Client {
    /// Compositor-specific handle, stable while the window lives.
    pub address: String,
    /// Owning process.
    pub pid: i32,
    /// Wayland `app_id` (Hyprland calls it `class`). This is the identity
    /// `AppSel::BundleId` and the deny list are keyed on.
    pub app_id: String,
    /// Current window title.
    pub title: String,
}

/// What `neo-ax` needs from a compositor.
pub(crate) trait WindowManager: Send + Sync {
    /// Which compositor this is, for error messages.
    fn name(&self) -> &'static str;

    /// Every mapped top-level window.
    ///
    /// # Errors
    ///
    /// When the compositor cannot be reached.
    fn clients(&self) -> Result<Vec<Client>, AxError>;

    /// The active window, which is Wayland's answer to "frontmost".
    ///
    /// # Errors
    ///
    /// When the compositor cannot be reached.
    fn frontmost(&self) -> Result<Option<Client>, AxError>;

    /// Give a window keyboard focus.
    ///
    /// # Errors
    ///
    /// When the compositor refuses or cannot be reached.
    fn focus(&self, client: &Client) -> Result<(), AxError>;
}

/// The compositor of this session, or a named refusal.
///
/// # Errors
///
/// [`AxError::NoWindowManager`] when nothing here can drive it.
pub(crate) fn detect() -> Result<Box<dyn WindowManager>, AxError> {
    if let Some(hyprland) = Hyprland::detect() {
        return Ok(Box::new(hyprland));
    }
    let compositor = std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("XDG_SESSION_DESKTOP"))
        .unwrap_or_else(|_| "unknown".to_owned());
    Err(AxError::NoWindowManager {
        detail: format!(
            "`{compositor}` is not one this build can drive (Hyprland is, through \
             $XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock)"
        ),
    })
}

/// Hyprland's IPC socket.
pub(crate) struct Hyprland {
    socket: std::path::PathBuf,
}

impl Hyprland {
    /// The socket for this session, when there is one.
    fn detect() -> Option<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
        let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
        let mut socket = std::path::PathBuf::from(runtime);
        socket.push("hypr");
        socket.push(signature);
        socket.push(".socket.sock");
        socket.exists().then_some(Self { socket })
    }

    fn request(&self, command: &str) -> Result<String, AxError> {
        let unreachable = || AxError::NoWindowManager {
            detail: format!(
                "Hyprland's socket at {} did not answer",
                self.socket.display()
            ),
        };
        let mut stream = UnixStream::connect(&self.socket).map_err(|_| unreachable())?;
        let timeout = Some(Duration::from_millis(500));
        let _ = stream.set_read_timeout(timeout);
        let _ = stream.set_write_timeout(timeout);
        stream
            .write_all(command.as_bytes())
            .map_err(|_| unreachable())?;
        let _ = stream.flush();
        let mut reply = String::new();
        stream
            .read_to_string(&mut reply)
            .map_err(|_| unreachable())?;
        Ok(reply)
    }

    fn client_of(value: &serde_json::Value) -> Option<Client> {
        let address = value.get("address")?.as_str()?.to_owned();
        let pid = i32::try_from(value.get("pid")?.as_i64()?).ok()?;
        if pid <= 0 {
            return None;
        }
        Some(Client {
            address,
            pid,
            app_id: value
                .get("class")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            title: value
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }
}

impl WindowManager for Hyprland {
    fn name(&self) -> &'static str {
        "Hyprland"
    }

    fn clients(&self) -> Result<Vec<Client>, AxError> {
        let body = self.request("j/clients")?;
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| AxError::NoWindowManager {
                detail: "Hyprland answered `j/clients` with something that is not JSON".to_owned(),
            })?;
        Ok(parsed
            .as_array()
            .map(|rows| rows.iter().filter_map(Hyprland::client_of).collect())
            .unwrap_or_default())
    }

    fn frontmost(&self) -> Result<Option<Client>, AxError> {
        let body = self.request("j/activewindow")?;
        let parsed: serde_json::Value =
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        Ok(Hyprland::client_of(&parsed))
    }

    fn focus(&self, client: &Client) -> Result<(), AxError> {
        // Newest form first. See the module comment: 0.56 evaluates the
        // payload as Lua and rejects the string dispatcher outright, while
        // releases before it know only the string form.
        let attempts = [
            format!(
                "/dispatch hl.dsp.focus({{ window = \"address:{}\" }})",
                client.address
            ),
            format!("/dispatch focuswindow address:{}", client.address),
        ];
        let mut last = String::new();
        for attempt in attempts {
            let reply = self.request(&attempt)?;
            if reply.trim_start().starts_with("ok") {
                return Ok(());
            }
            last = reply;
        }
        Err(AxError::NoWindowManager {
            detail: format!(
                "Hyprland refused to focus {}: {}",
                client.address,
                last.trim()
            ),
        })
    }
}
