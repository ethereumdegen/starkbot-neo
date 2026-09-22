//! Control-socket window requests complete only after the webview applies them.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tokio::sync::oneshot;

use crate::error::UiError;

const CHANNEL: &str = "ui://window-mode";
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PENDING: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Mini,
    Full,
    Toggle,
    Status,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    Mini,
    Full,
}

struct Pending {
    desired: WindowAction,
    reply: oneshot::Sender<Result<WindowMode, UiError>>,
}

#[derive(Default)]
struct Requests {
    next: u32,
    closed: bool,
    pending: HashMap<u32, Pending>,
}

#[derive(Default)]
pub struct ModeControl {
    requests: Mutex<Requests>,
}

impl ModeControl {
    fn lock(&self) -> MutexGuard<'_, Requests> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub async fn request<R: tauri::Runtime>(
        &self,
        app: &AppHandle<R>,
        desired: WindowAction,
    ) -> Result<WindowMode, UiError> {
        if app.get_webview_window("main").is_none() {
            return Err(UiError::new(
                "window_closed",
                "the Starkbot main window is not open",
            ));
        }
        let (reply, receiver) = oneshot::channel();
        let id = {
            let mut requests = self.lock();
            if requests.closed {
                return Err(UiError::new(
                    "window_closed",
                    "the Starkbot main window has closed",
                ));
            }
            if requests.pending.len() >= MAX_PENDING {
                return Err(UiError::new(
                    "window_busy",
                    "too many window-mode requests are waiting; try again after they finish",
                ));
            }
            requests.next = requests.next.checked_add(1).ok_or_else(|| {
                UiError::new(
                    "window_busy",
                    "window request identifiers are exhausted; restart the desktop app",
                )
            })?;
            let id = requests.next;
            requests.pending.insert(id, Pending { desired, reply });
            id
        };
        // Also removes the waiter if its caller is cancelled before completion.
        let _pending = RequestGuard { control: self, id };
        app.emit_to("main", CHANNEL, (id, desired))
            .map_err(|error| {
                UiError::new(
                    "window_event",
                    format!("could not reach the Starkbot window: {error}"),
                )
            })?;
        tokio::time::timeout(TIMEOUT, receiver)
            .await
            .map_err(|_| UiError::new("window_timeout", "the Starkbot window did not confirm its mode within 10 seconds; check that the desktop UI is responsive"))?
            .map_err(|_| UiError::new("window_closed", "the Starkbot window closed before confirming its mode"))?
    }

    pub fn complete(
        &self,
        id: u32,
        actual: WindowMode,
        error: Option<String>,
    ) -> Result<(), UiError> {
        let pending = self.lock().pending.remove(&id).ok_or_else(|| {
            UiError::new(
                "window_request_missing",
                "this window-mode request already finished or expired",
            )
        })?;
        let result = if let Some(error) = error {
            Err(UiError::new("window_mode", error))
        } else if matches!(
            (pending.desired, actual),
            (WindowAction::Mini, WindowMode::Full) | (WindowAction::Full, WindowMode::Mini)
        ) {
            Err(UiError::new(
                "window_mode",
                "the Starkbot window did not reach the requested mode",
            ))
        } else {
            Ok(actual)
        };
        let _ = pending.reply.send(result);
        Ok(())
    }

    /// Fail all outstanding callers immediately when the main window closes.
    pub fn shutdown(&self) {
        let pending = {
            let mut requests = self.lock();
            requests.closed = true;
            std::mem::take(&mut requests.pending)
        };
        for (_, pending) in pending {
            let _ = pending.reply.send(Err(UiError::new(
                "window_closed",
                "the Starkbot main window closed before confirming its mode",
            )));
        }
    }
}

struct RequestGuard<'a> {
    control: &'a ModeControl,
    id: u32,
}

impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        self.control.lock().pending.remove(&self.id);
    }
}

#[tauri::command]
pub fn complete_window_mode<R: tauri::Runtime>(
    window: WebviewWindow<R>,
    control: State<'_, ModeControl>,
    request_id: u32,
    actual_mode: WindowMode,
    error: Option<String>,
) -> Result<(), UiError> {
    if window.label() != "main" {
        return Err(UiError::new(
            "window_mode",
            "only the main Starkbot window can confirm its mode",
        ));
    }
    control.complete(request_id, actual_mode, error)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use tauri::Listener;
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};

    fn app() -> tauri::App<MockRuntime> {
        let app = mock_builder()
            .manage(ModeControl::default())
            .build(mock_context(noop_assets()))
            .expect("mock app");
        tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::App("index.html".into()))
            .build()
            .expect("main window");
        app
    }

    #[tokio::test]
    async fn emitting_is_not_success_and_only_matching_acknowledgement_completes() {
        let app = app();
        let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
        app.listen_any(CHANNEL, move |event| {
            let request: (u32, WindowAction) =
                serde_json::from_str(event.payload()).expect("request");
            let _ = send.send(request);
        });
        let handle = app.handle().clone();
        let pending = tokio::spawn(async move {
            handle
                .state::<ModeControl>()
                .request(&handle, WindowAction::Mini)
                .await
        });
        let (id, _) = tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await
            .expect("request delivered")
            .expect("request event");
        assert!(
            !pending.is_finished(),
            "emitting the event does not change the window"
        );
        assert_eq!(
            app.state::<ModeControl>()
                .complete(id + 1, WindowMode::Mini, None)
                .expect_err("an unrelated ack cannot complete this request")
                .code,
            "window_request_missing",
        );
        app.state::<ModeControl>()
            .complete(id, WindowMode::Mini, None)
            .expect("acknowledged");
        assert_eq!(
            pending.await.expect("request task").expect("actual mode"),
            WindowMode::Mini
        );

        let handle = app.handle().clone();
        let refused = tokio::spawn(async move {
            handle
                .state::<ModeControl>()
                .request(&handle, WindowAction::Mini)
                .await
        });
        let (id, _) = tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await
            .expect("request delivered")
            .expect("request event");
        app.state::<ModeControl>()
            .complete(id, WindowMode::Full, None)
            .expect("acknowledged");
        assert_eq!(
            refused
                .await
                .expect("request task")
                .expect_err("the requested mode was not reached")
                .code,
            "window_mode",
        );
    }

    #[tokio::test]
    async fn closing_the_window_fails_every_waiting_request() {
        let app = app();
        let (send, mut requests) = tokio::sync::mpsc::unbounded_channel();
        app.listen_any(CHANNEL, move |_| {
            let _ = send.send(());
        });
        let mut tasks = Vec::new();
        for desired in [WindowAction::Toggle, WindowAction::Status] {
            let handle = app.handle().clone();
            tasks.push(tokio::spawn(async move {
                handle
                    .state::<ModeControl>()
                    .request(&handle, desired)
                    .await
            }));
        }
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(3), requests.recv())
                .await
                .expect("request delivered")
                .expect("request event");
        }
        app.state::<ModeControl>().shutdown();
        for task in tasks {
            assert_eq!(
                task.await
                    .expect("request task")
                    .expect_err("window is closed")
                    .code,
                "window_closed",
            );
        }
    }
}
