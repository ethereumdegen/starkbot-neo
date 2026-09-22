//! Application-local compositor support where GTK cannot position a Wayland window.
//! No window rules, shell commands, or other clients' properties are changed.

use crate::error::UiError;

#[derive(Default)]
pub struct WindowMode {
    #[cfg(target_os = "linux")]
    snapshot: std::sync::Arc<std::sync::Mutex<Option<hyprland::Snapshot>>>,
}

#[tauri::command]
pub async fn window_compositor(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, WindowMode>,
    action: String,
) -> Result<bool, UiError> {
    if window.label() != "main" {
        return Err(UiError::new("window_mode", "Only the main window can change mini mode."));
    }
    #[cfg(target_os = "linux")]
    {
        let snapshot = state.snapshot.clone();
        tokio::task::spawn_blocking(move || hyprland::transition(&snapshot, &action))
            .await
            .map_err(UiError::from)?
            .map_err(|message| UiError::new("window_mode", message))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, action);
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
mod hyprland {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Client {
        address: String,
        pid: i64,
        at: [i32; 2],
        size: [i32; 2],
        floating: bool,
        monitor: i64,
        fullscreen: u32,
        #[serde(rename = "fullscreenClient")]
        fullscreen_client: u32,
    }

    #[derive(Deserialize)]
    struct Monitor {
        id: i64,
        x: i32,
        y: i32,
        width: f64,
        height: f64,
        scale: f64,
        transform: u32,
        reserved: [i32; 4],
    }

    pub(super) struct Snapshot {
        client: Client,
        normal_bounds: Option<([i32; 2], [i32; 2])>,
    }

    fn request(socket: &Path, command: &str) -> Result<String, String> {
        let mut stream = UnixStream::connect(socket)
            .map_err(|error| format!("Cannot connect to Hyprland for mini mode: {error}"))?;
        let timeout = Some(Duration::from_secs(2));
        stream.set_read_timeout(timeout).map_err(|error| error.to_string())?;
        stream.set_write_timeout(timeout).map_err(|error| error.to_string())?;
        stream.write_all(command.as_bytes()).map_err(|error| error.to_string())?;
        let mut response = String::new();
        stream.take(4 * 1024 * 1024).read_to_string(&mut response)
            .map_err(|error| format!("Hyprland mini mode request failed: {error}"))?;
        Ok(response)
    }

    fn dispatch(socket: &Path, command: &str) -> Result<(), String> {
        // Lua dispatch returns a result table; eval itself succeeding does not
        // mean the dispatcher succeeded. Turn a rejected request into an IPC error.
        let response = request(socket, &format!(
            "/eval local result = hl.dispatch({command}); if not result or result.ok ~= true then error(result and result.error or 'Window dispatcher failed') end"
        ))?;
        if response.trim() == "ok" {
            Ok(())
        } else {
            Err(format!("Hyprland could not {command}: {}", response.trim()))
        }
    }

    fn own_client(socket: &Path, address: Option<&str>) -> Result<Client, String> {
        let clients: Vec<Client> = serde_json::from_str(&request(socket, "j/clients")?)
            .map_err(|error| format!("Cannot read Hyprland windows: {error}"))?;
        let mut own = clients.into_iter().filter(|client| {
            client.pid == i64::from(std::process::id())
                && address.is_none_or(|address| client.address == address)
        });
        let client = own.next().ok_or("Hyprland cannot locate this app's main window.")?;
        if own.next().is_some() {
            return Err("Hyprland found more than one main window for this process.".into());
        }
        // Only a compositor-returned, validated address is interpolated into dispatchers.
        if !client.address.starts_with("0x")
            || !client.address[2..].chars().all(|character| character.is_ascii_hexdigit())
            || client.address.len() <= 2
        {
            return Err("Hyprland returned an invalid window address.".into());
        }
        Ok(client)
    }

    fn socket_path() -> Result<Option<PathBuf>, String> {
        let Some(signature) = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE") else {
            let x11 = std::env::var("GDK_BACKEND").is_ok_and(|backend| backend == "x11");
            if std::env::var_os("WAYLAND_DISPLAY").is_some() && !x11 {
                return Err("This Wayland compositor does not expose application window placement. Mini mode currently supports Hyprland, X11, macOS and Windows; use an X11 session/backend on other Linux compositors.".into());
            }
            return Ok(None);
        };
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .ok_or("XDG_RUNTIME_DIR is missing; cannot locate Hyprland's control socket.")?;
        Ok(Some(PathBuf::from(runtime).join("hypr").join(signature).join(".socket.sock")))
    }

    fn enter(socket: &Path, snapshot: &mut Snapshot) -> Result<(), String> {
        let mut current = own_client(socket, Some(&snapshot.client.address))?;
        let target = format!("address:{}", current.address);
        dispatch(socket, &format!(
            "hl.dsp.window.fullscreen_state({{ internal = 0, client = 0, action = 'set', window = '{target}' }})"
        ))?;
        current = own_client(socket, Some(&snapshot.client.address))?;
        if snapshot.normal_bounds.is_none() {
            snapshot.normal_bounds = Some((current.at, current.size));
        }
        let monitors: Vec<Monitor> = serde_json::from_str(&request(socket, "j/monitors")?)
            .map_err(|error| format!("Cannot read Hyprland monitors: {error}"))?;
        let monitor = monitors.into_iter().find(|monitor| monitor.id == current.monitor)
            .ok_or("The current monitor is no longer available.")?;
        if !monitor.scale.is_finite() || monitor.scale <= 0.0 {
            return Err("Hyprland returned an invalid monitor scale.".into());
        }
        let (physical_width, physical_height) = if monitor.transform % 2 == 0 {
            (monitor.width, monitor.height)
        } else {
            (monitor.height, monitor.width)
        };
        let width = (physical_width / monitor.scale).round() as i32
            - monitor.reserved[0] - monitor.reserved[2];
        let height = (physical_height / monitor.scale).round() as i32
            - monitor.reserved[1] - monitor.reserved[3];
        if width < 420 || height < 180 {
            return Err("The monitor work area is too small for mini mode (420 × 180 minimum).".into());
        }
        let mini_width = 560.min(width);
        let mini_height = 220.min(height);
        let x = monitor.x + monitor.reserved[0] + (width - mini_width) / 2;
        let y = monitor.y + monitor.reserved[1] + (height - mini_height - 24).max(0);
        dispatch(socket, &format!("hl.dsp.window.float({{ action = 'on', window = '{target}' }})"))?;
        dispatch(socket, &format!("hl.dsp.window.resize({{ x = {mini_width}, y = {mini_height}, window = '{target}' }})"))?;
        dispatch(socket, &format!("hl.dsp.window.move({{ x = {x}, y = {y}, window = '{target}' }})"))?;
        // Wayland has no keep-above request. Raising a floating window is supported;
        // another floating window can still cover it when the user focuses that window.
        dispatch(socket, &format!("hl.dsp.window.alter_zorder({{ mode = 'top', window = '{target}' }})"))?;
        let placed = own_client(socket, Some(&current.address))?;
        if !placed.floating {
            return Err("Hyprland did not allow this window to float.".into());
        }
        Ok(())
    }

    fn restore(socket: &Path, snapshot: &Snapshot) -> Result<(), String> {
        let current = own_client(socket, Some(&snapshot.client.address))?;
        let target = format!("address:{}", current.address);
        dispatch(socket, &format!(
            "hl.dsp.window.fullscreen_state({{ internal = 0, client = 0, action = 'set', window = '{target}' }})"
        ))?;
        let (at, size) = if snapshot.client.fullscreen == 0 {
            (snapshot.client.at, snapshot.client.size)
        } else {
            snapshot.normal_bounds.unwrap_or((snapshot.client.at, snapshot.client.size))
        };
        if snapshot.client.floating {
            dispatch(socket, &format!("hl.dsp.window.float({{ action = 'on', window = '{target}' }})"))?;
            dispatch(socket, &format!("hl.dsp.window.resize({{ x = {}, y = {}, window = '{target}' }})", size[0], size[1]))?;
            dispatch(socket, &format!("hl.dsp.window.move({{ x = {}, y = {}, window = '{target}' }})", at[0], at[1]))?;
        } else {
            dispatch(socket, &format!("hl.dsp.window.float({{ action = 'off', window = '{target}' }})"))?;
            dispatch(socket, &format!("hl.dsp.window.resize({{ x = {}, y = {}, window = '{target}' }})", size[0], size[1]))?;
        }
        dispatch(socket, &format!(
            "hl.dsp.window.fullscreen_state({{ internal = {}, client = {}, action = 'set', window = '{target}' }})",
            snapshot.client.fullscreen, snapshot.client.fullscreen_client
        ))?;
        Ok(())
    }

    pub(super) fn transition(state: &Mutex<Option<Snapshot>>, action: &str) -> Result<bool, String> {
        let Some(socket) = socket_path()? else { return Ok(false) };
        let mut saved = state.lock().map_err(|_| "Mini mode window state is unavailable.")?;
        match action {
            "capture" => {
                if saved.is_none() {
                    *saved = Some(Snapshot { client: own_client(&socket, None)?, normal_bounds: None });
                }
            }
            "mini" => enter(&socket, saved.as_mut().ok_or("Full window state was not captured.")?)?,
            "restore" => {
                if let Some(snapshot) = saved.as_ref() {
                    restore(&socket, snapshot)?;
                }
            }
            "clear" => *saved = None,
            _ => return Err("Unknown mini mode window operation.".into()),
        }
        Ok(true)
    }
}
