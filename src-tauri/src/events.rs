//! The core's event stream, forwarded into the webview.
//!
//! One subscription for the whole process and one event name, because a
//! webview cannot hold a `broadcast::Receiver`: every pane listens to
//! `app://event` and filters by the run id the payload carries. The payload
//! is a `neo_core::Envelope` serialised verbatim — no view type, no second
//! vocabulary to keep in step with the core's.

use std::sync::Arc;

use neo_agent::Runtime;
use neo_core::{AppEvent, Envelope, NoticeLevel};
use tauri::{AppHandle, Emitter};
use time::OffsetDateTime;
use tokio::sync::broadcast::error::RecvError;

/// The one event the webview listens to.
pub const CHANNEL: &str = "app://event";

/// The code a front end watches for to know it must re-bootstrap.
pub const GAP: &str = "event_gap";

/// Start forwarding. Runs until the app handle or the core's channel goes
/// away, which is process exit either way.
pub fn forward<R: tauri::Runtime>(app: AppHandle<R>, runtime: &Arc<Runtime>) {
    // Subscribed here rather than inside the task: a subscription taken after
    // the first command has run would miss whatever that command published.
    let mut events = runtime.subscribe();
    tauri::async_runtime::spawn(async move {
        let mut last = 0_u64;
        loop {
            match events.recv().await {
                Ok(envelope) => {
                    last = envelope.seq;
                    if app.emit(CHANNEL, &envelope).is_err() {
                        // The window is gone; there is nobody to tell.
                        break;
                    }
                }
                // The webview fell behind and the channel dropped the oldest
                // events. Saying nothing would leave it rendering a thread it
                // cannot trust, and `Lagged` never reaches JavaScript on its
                // own, so the gap is announced in the stream itself.
                Err(RecvError::Lagged(missed)) => {
                    if app.emit(CHANNEL, gap(last, missed)).is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// The marker a lagging subscriber gets, as an ordinary envelope so the front
/// end needs no second parser.
///
/// `seq` repeats the last sequence actually delivered: it is deliberately not
/// a new number, because inventing one would hide the very discontinuity this
/// exists to report.
pub(crate) fn gap(last: u64, missed: u64) -> Envelope {
    Envelope {
        seq: last,
        at: OffsetDateTime::now_utc(),
        event: AppEvent::Notice {
            level: NoticeLevel::Warning,
            code: GAP.to_owned(),
            text: format!(
                "the window fell behind and missed {missed} event(s); reload the screen from `get_bootstrap`"
            ),
        },
    }
}
