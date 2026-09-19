use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "macos")]
use block2::RcBlock;
#[cfg(target_os = "macos")]
use image::imageops::FilterType;
#[cfg(target_os = "macos")]
use objc2_app_kit::NSImage;
#[cfg(target_os = "macos")]
use objc2_foundation::NSError;
#[cfg(target_os = "macos")]
use objc2_web_kit::WKWebView;
use s7_render::fixtures::{Fixture, METRICS_EXPRESSION, engine_drift_fixtures};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, State, WebviewWindow};

const FIXTURE_SIZE: u32 = 1080;

struct RunState {
    report_path: PathBuf,
    capture_dir: Option<PathBuf>,
}

#[derive(Serialize)]
struct FixtureBundle {
    fixtures: Vec<Fixture>,
    metrics_expression: &'static str,
    capture_enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureResult {
    id: String,
    path: String,
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
}

#[tauri::command]
fn fixture_bundle(state: State<'_, RunState>) -> FixtureBundle {
    FixtureBundle {
        fixtures: engine_drift_fixtures(),
        metrics_expression: METRICS_EXPRESSION,
        capture_enabled: state.capture_dir.is_some(),
    }
}

#[cfg(target_os = "macos")]
#[tauri::command]
async fn capture_fixture(
    window: WebviewWindow,
    state: State<'_, RunState>,
    id: String,
) -> Result<CaptureResult, String> {
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("invalid fixture id {id:?}"));
    }
    let output_dir = state
        .capture_dir
        .clone()
        .ok_or_else(|| "snapshot capture was not enabled".to_owned())?;
    let (sender, receiver) = tokio::sync::oneshot::channel::<Result<Vec<u8>, String>>();
    let sender = Arc::new(Mutex::new(Some(sender)));

    window
        .with_webview(move |platform| {
            let sender = Arc::clone(&sender);
            let completion = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                let result = if image.is_null() {
                    if error.is_null() {
                        Err("WKWebView returned neither an image nor an error".to_owned())
                    } else {
                        Err("WKWebView snapshot failed".to_owned())
                    }
                } else {
                    // SAFETY: WebKit owns the NSImage for the duration of this callback. The TIFF
                    // bytes are copied before the callback returns.
                    let image = unsafe { &*image };
                    image
                        .TIFFRepresentation()
                        .map(|data| data.to_vec())
                        .ok_or_else(|| "NSImage did not provide a TIFF representation".to_owned())
                };
                if let Some(sender) = sender.lock().expect("snapshot sender poisoned").take() {
                    let _ = sender.send(result);
                }
            });

            // SAFETY: Tauri exposes the retained Wry WKWebView as this Objective-C pointer.
            // Wry's private subclass has WKWebView as its superclass, so the cast is valid.
            let webview = unsafe { &*(platform.inner().cast::<WKWebView>()) };
            unsafe {
                webview.takeSnapshotWithConfiguration_completionHandler(None, &completion);
            }
        })
        .map_err(|error| format!("could not access WKWebView: {error}"))?;

    let tiff = receiver
        .await
        .map_err(|_| "WKWebView dropped the snapshot callback".to_owned())??;
    let source = image::load_from_memory_with_format(&tiff, image::ImageFormat::Tiff)
        .map_err(|error| format!("could not decode WKWebView snapshot: {error}"))?;
    let source_width = source.width();
    let source_height = source.height();
    let normalized = if source_width == FIXTURE_SIZE && source_height == FIXTURE_SIZE {
        source
    } else {
        source.resize_exact(FIXTURE_SIZE, FIXTURE_SIZE, FilterType::Lanczos3)
    };

    std::fs::create_dir_all(&output_dir).map_err(|error| error.to_string())?;
    let path = output_dir.join(format!("{id}.png"));
    normalized
        .save_with_format(&path, image::ImageFormat::Png)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;

    Ok(CaptureResult {
        id,
        path: path.display().to_string(),
        source_width,
        source_height,
        width: FIXTURE_SIZE,
        height: FIXTURE_SIZE,
    })
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
async fn capture_fixture(
    _window: WebviewWindow,
    _state: State<'_, RunState>,
    _id: String,
) -> Result<CaptureResult, String> {
    Err("native WKWebView capture is available only on macOS".to_owned())
}

#[tauri::command]
fn report(app: AppHandle, state: State<'_, RunState>, report: Value) -> Result<(), String> {
    if let Some(parent) = state.report_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    std::fs::write(&state.report_path, bytes).map_err(|error| error.to_string())?;
    println!(
        "WKWebView fidelity report → {}",
        state.report_path.display()
    );
    app.exit(0);
    Ok(())
}

fn main() {
    let report_path = std::env::var_os("NEO_WEBKIT_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("spikes/out/engine-drift/webkit.json"));
    let capture_dir = std::env::var_os("NEO_WEBKIT_CAPTURE_DIR").map(PathBuf::from);
    tauri::Builder::default()
        .manage(RunState {
            report_path,
            capture_dir,
        })
        .invoke_handler(tauri::generate_handler![
            fixture_bundle,
            capture_fixture,
            report
        ])
        .run(tauri::generate_context!())
        .expect("failed to run WKWebView fidelity spike");
}
