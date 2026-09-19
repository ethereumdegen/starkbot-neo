use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use block2::RcBlock;
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use neo_cdp::{Browser, LaunchOptions};
use objc2_app_kit::{NSApplication, NSImage, NSWorkspace};
use objc2_foundation::NSError;
use objc2_web_kit::WKWebView;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Position, Size, State, WebviewUrl,
    WebviewWindow,
};
use tauri_nspanel::{
    CollectionBehavior, ManagerExt, Panel, PanelBuilder, PanelLevel, StyleMask, tauri_panel,
};

const CHROME_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
*{box-sizing:border-box}html,body{margin:0;width:100%;height:100%;overflow:hidden}body{display:grid;place-items:center;background:radial-gradient(circle at 70% 20%,#3e237f,#090a16 62%);font-family:-apple-system,sans-serif;color:white}.target{width:270px;height:180px;border:0;border-radius:24px;background:#f7f5ff;color:#171423;font-size:24px;font-weight:800;box-shadow:0 30px 100px #0009}.label{position:fixed;left:52px;bottom:42px;font-size:18px;letter-spacing:.14em;color:#aaa4c5}
</style></head><body><button class="target" onclick="window.__clicks++">FULLSCREEN TARGET<br><small>click-through probe</small></button><div class="label">MANAGED CHROME · FULLSCREEN TEST SPACE</div><script>window.__clicks=0</script></body></html>"#;

#[derive(Default)]
struct RunState {
    pill_clicks: AtomicU64,
    quick_value: Mutex<String>,
}

#[derive(Clone, Copy)]
struct PanelGeometry {
    pill_center: (f64, f64),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppIdentity {
    name: String,
    bundle_id: String,
    pid: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PanelFacts {
    visible: bool,
    key: bool,
    can_become_key: bool,
    can_become_main: bool,
    floating: bool,
    becomes_key_only_if_needed: bool,
    hides_on_deactivate: bool,
    ignores_mouse_events: bool,
    level: i64,
    collection_behavior: u64,
    style_mask: u64,
    frame: [f64; 4],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    frontmost: AppIdentity,
    activation_policy: String,
    pill: PanelFacts,
    ring: PanelFacts,
    quick_entry: PanelFacts,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpikeReport {
    passed: bool,
    duration_ms: u128,
    chrome_window_state: String,
    chrome_viewport: Value,
    before_show: Observation,
    after_show: Observation,
    after_pill_click: Observation,
    quick_entry_open: Observation,
    after_quick_entry: Observation,
    after_policy_cycle: Observation,
    chrome_clicks_through_ring: u64,
    pill_clicks: u64,
    quick_entry_value: String,
    hide_show_cycle: bool,
    panel_snapshots: Vec<String>,
    panel_snapshots_captured: bool,
    checks: Vec<Check>,
}

tauri_panel! {
    panel!(PassivePanel {
        config: {
            can_become_key_window: false,
            can_become_main_window: false,
            becomes_key_only_if_needed: true,
            is_floating_panel: true,
            hides_on_deactivate: false
        }
    })

    panel!(QuickEntryPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            becomes_key_only_if_needed: true,
            is_floating_panel: true,
            hides_on_deactivate: false
        }
    })
}

#[tauri::command]
fn pill_clicked(state: State<'_, RunState>) {
    state.pill_clicks.fetch_add(1, Ordering::Relaxed);
}

#[tauri::command]
fn quick_changed(state: State<'_, RunState>, value: String) {
    *state
        .quick_value
        .lock()
        .expect("quick value mutex poisoned") = value;
}

fn main() {
    let report_path = std::env::var_os("NEO_PANEL_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("spikes/out/s6-panel/report.json"));

    tauri::Builder::default()
        .plugin(tauri_nspanel::init())
        .manage(RunState::default())
        .invoke_handler(tauri::generate_handler![pill_clicked, quick_changed])
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let geometry = build_panels(app.app_handle())?;
            app.app_handle()
                .set_activation_policy(tauri::ActivationPolicy::Prohibited)?;
            let handle = app.app_handle().clone();
            let path = report_path.clone();
            tauri::async_runtime::spawn(async move {
                let outcome = run_probe(handle.clone(), geometry, &path).await;
                let exit_code = match outcome {
                    Ok(report) => {
                        let passed = report.passed;
                        if let Err(error) = write_json(&path, &report) {
                            eprintln!("could not write NSPanel report: {error:#}");
                            1
                        } else {
                            println!("NSPanel spike report → {}", path.display());
                            if passed { 0 } else { 1 }
                        }
                    }
                    Err(error) => {
                        let failure = json!({ "passed": false, "error": format!("{error:#}") });
                        let _ = write_json(&path, &failure);
                        eprintln!("NSPanel spike failed: {error:#}");
                        1
                    }
                };
                handle.exit(exit_code);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run NSPanel spike");
}

fn build_panels(app: &AppHandle) -> tauri::Result<PanelGeometry> {
    let monitor = app
        .primary_monitor()?
        .expect("NSPanel spike requires a primary display");
    let scale = monitor.scale_factor();
    let origin = monitor.position();
    let size = monitor.size();
    let screen_x = f64::from(origin.x) / scale;
    let screen_y = f64::from(origin.y) / scale;
    let screen_width = f64::from(size.width) / scale;
    let screen_height = f64::from(size.height) / scale;
    let behavior = || {
        CollectionBehavior::new()
            .full_screen_auxiliary()
            .can_join_all_spaces()
            .stationary()
            .ignores_cycle()
    };
    let passive_style = || StyleMask::empty().borderless().nonactivating_panel();

    let pill_width = 390.0;
    let pill_height = 72.0;
    let pill_x = screen_x + screen_width - pill_width - 34.0;
    let pill_y = screen_y + 34.0;
    PanelBuilder::<_, PassivePanel>::new(app, "pill")
        .url(WebviewUrl::App("index.html?panel=pill".into()))
        .position(Position::Logical(LogicalPosition::new(pill_x, pill_y)))
        .size(Size::Logical(LogicalSize::new(pill_width, pill_height)))
        .no_activate(true)
        .level(PanelLevel::PopUpMenu)
        .floating(true)
        .hides_on_deactivate(false)
        .becomes_key_only_if_needed(true)
        .style_mask(passive_style())
        .collection_behavior(behavior())
        .transparent(true)
        .opaque(false)
        .has_shadow(true)
        .with_window(|window| {
            window
                .decorations(false)
                .resizable(false)
                .visible(false)
                .skip_taskbar(true)
        })
        .build()?;

    PanelBuilder::<_, PassivePanel>::new(app, "ring")
        .url(WebviewUrl::App("index.html?panel=ring".into()))
        .position(Position::Logical(LogicalPosition::new(screen_x, screen_y)))
        .size(Size::Logical(LogicalSize::new(screen_width, screen_height)))
        .no_activate(true)
        .level(PanelLevel::Status)
        .floating(true)
        .ignores_mouse_events(true)
        .hides_on_deactivate(false)
        .style_mask(passive_style())
        .collection_behavior(behavior())
        .transparent(true)
        .opaque(false)
        .has_shadow(false)
        .with_window(|window| {
            window
                .decorations(false)
                .resizable(false)
                .visible(false)
                .skip_taskbar(true)
        })
        .build()?;

    let quick_width = 620.0;
    let quick_height = 96.0;
    PanelBuilder::<_, QuickEntryPanel>::new(app, "quick-entry")
        .url(WebviewUrl::App("index.html?panel=quick".into()))
        .position(Position::Logical(LogicalPosition::new(
            screen_x + (screen_width - quick_width) / 2.0,
            screen_y + screen_height / 5.0,
        )))
        .size(Size::Logical(LogicalSize::new(quick_width, quick_height)))
        .no_activate(true)
        .level(PanelLevel::PopUpMenu)
        .floating(true)
        .hides_on_deactivate(false)
        .becomes_key_only_if_needed(true)
        .style_mask(StyleMask::empty().borderless().nonactivating_panel())
        .collection_behavior(behavior())
        .transparent(true)
        .opaque(false)
        .has_shadow(true)
        .with_window(|window| {
            window
                .decorations(false)
                .resizable(false)
                .visible(false)
                .skip_taskbar(true)
        })
        .build()?;

    Ok(PanelGeometry {
        pill_center: (pill_x + pill_width / 2.0, pill_y + pill_height / 2.0),
    })
}

async fn run_probe(
    app: AppHandle,
    geometry: PanelGeometry,
    report_path: &Path,
) -> anyhow::Result<SpikeReport> {
    let started = Instant::now();
    let profile = tempfile::tempdir()?;
    let mut launch = LaunchOptions::new(profile.path());
    launch.headless = false;
    launch.window = (1280, 900);
    let browser = Browser::launch(&launch).await?;
    let page = browser.new_page("about:blank").await?;
    let html = serde_json::to_string(CHROME_HTML)?;
    page.evaluate(&format!(
        "document.open();document.write({html});document.close();true"
    ))
    .await?;
    page.activate().await?;

    let window = browser
        .call(
            "Browser.getWindowForTarget",
            json!({ "targetId": page.target_id() }),
        )
        .await?;
    let window_id = window["windowId"]
        .as_i64()
        .context("Chrome did not return a window id")?;
    browser
        .call(
            "Browser.setWindowBounds",
            json!({ "windowId": window_id, "bounds": { "windowState": "fullscreen" } }),
        )
        .await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let bounds = browser
        .call("Browser.getWindowBounds", json!({ "windowId": window_id }))
        .await?;
    let chrome_window_state = bounds["bounds"]["windowState"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    let chrome_viewport = page
        .evaluate(
            "({innerWidth,innerHeight,outerWidth,outerHeight,screenX,screenY,devicePixelRatio})",
        )
        .await?;

    let before_show = wait_for_chrome_frontmost(&app).await?;
    panel_action(&app, PanelAction::ShowPassive).await?;
    tokio::time::sleep(Duration::from_millis(900)).await;
    let after_show = observe(&app).await?;
    let artifact_dir = report_path.parent().unwrap_or_else(|| Path::new("."));
    let pill_snapshot = artifact_dir.join("pill.png");
    let ring_snapshot = artifact_dir.join("ring.png");
    capture_panel(&app, "pill", &pill_snapshot).await?;
    capture_panel(&app, "ring", &ring_snapshot).await?;
    let panel_snapshots = vec![
        pill_snapshot.display().to_string(),
        ring_snapshot.display().to_string(),
    ];
    let panel_snapshots_captured = panel_snapshots.iter().all(|path| Path::new(path).is_file());

    let target = page
        .evaluate("(()=>{const r=document.querySelector('.target').getBoundingClientRect();return{x:screenX+(outerWidth-innerWidth)/2+r.x+r.width/2,y:screenY+(outerHeight-innerHeight)+r.y+r.height/2}})()")
        .await?;
    let target_x = target["x"].as_f64().context("missing target x")?;
    let target_y = target["y"].as_f64().context("missing target y")?;
    post_click(target_x, target_y)?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let chrome_clicks_through_ring = page
        .evaluate("window.__clicks")
        .await?
        .as_u64()
        .unwrap_or_default();

    post_click(geometry.pill_center.0, geometry.pill_center.1)?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let pill_clicks = app.state::<RunState>().pill_clicks.load(Ordering::Relaxed);
    let after_pill_click = observe(&app).await?;

    panel_action(&app, PanelAction::ShowQuickEntry).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    panel_action(&app, PanelAction::FocusQuickEntry).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let quick_entry_open = observe(&app).await?;
    post_text("neo")?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let quick_entry_value = app
        .state::<RunState>()
        .quick_value
        .lock()
        .expect("quick value mutex poisoned")
        .clone();
    panel_action(&app, PanelAction::HideQuickEntry).await?;
    page.activate().await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_quick_entry = observe(&app).await?;

    panel_action(&app, PanelAction::HidePill).await?;
    let hidden = !observe(&app).await?.pill.visible;
    panel_action(&app, PanelAction::ShowPill).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let hide_show_cycle = hidden && observe(&app).await?.pill.visible;

    app.set_activation_policy(tauri::ActivationPolicy::Regular)
        .context("could not switch activation policy to Regular")?;
    tokio::time::sleep(Duration::from_millis(350)).await;
    app.set_activation_policy(tauri::ActivationPolicy::Prohibited)
        .context("could not restore Prohibited activation policy")?;
    tokio::time::sleep(Duration::from_millis(350)).await;
    panel_action(&app, PanelAction::ShowPassive).await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let after_policy_cycle = observe(&app).await?;

    let chrome_identity = &before_show.frontmost;
    let mut checks = vec![
        check(
            "chrome entered fullscreen",
            chrome_window_state == "fullscreen",
            chrome_window_state.clone(),
        ),
        check(
            "pill and ring visible above fullscreen",
            after_show.pill.visible && after_show.ring.visible,
            format!(
                "pill={}, ring={}, levels={}/{}",
                after_show.pill.visible,
                after_show.ring.visible,
                after_show.pill.level,
                after_show.ring.level
            ),
        ),
        check(
            "passive panels cannot become key",
            !after_show.pill.can_become_key
                && !after_show.ring.can_become_key
                && !after_show.pill.key
                && !after_show.ring.key,
            format!(
                "pill can/key={}/{}, ring can/key={}/{}",
                after_show.pill.can_become_key,
                after_show.pill.key,
                after_show.ring.can_become_key,
                after_show.ring.key
            ),
        ),
        check(
            "showing panels preserved Chrome focus",
            same_app(chrome_identity, &after_show.frontmost),
            format!("frontmost={:?}", after_show.frontmost),
        ),
        check(
            "ring passes a real system click through",
            chrome_clicks_through_ring == 1,
            format!("Chrome click count={chrome_clicks_through_ring}"),
        ),
        check(
            "pill receives clicks without activation",
            pill_clicks == 1 && same_app(chrome_identity, &after_pill_click.frontmost),
            format!(
                "pill clicks={pill_clicks}, frontmost={:?}",
                after_pill_click.frontmost
            ),
        ),
        check(
            "quick entry takes key focus and receives text",
            quick_entry_open.quick_entry.key && quick_entry_value == "neo",
            format!(
                "key={}, value={quick_entry_value:?}",
                quick_entry_open.quick_entry.key
            ),
        ),
        check(
            "quick entry returns focus to Chrome",
            same_app(chrome_identity, &after_quick_entry.frontmost)
                && !after_quick_entry.quick_entry.visible,
            format!("frontmost={:?}", after_quick_entry.frontmost),
        ),
        check(
            "ring is configured click-through",
            after_show.ring.ignores_mouse_events,
            format!(
                "ignoresMouseEvents={}",
                after_show.ring.ignores_mouse_events
            ),
        ),
        check(
            "panel hide/show cycle remains healthy",
            hide_show_cycle,
            format!("cycle={hide_show_cycle}"),
        ),
        check(
            "activation policy cycle preserved focus and panels recover",
            same_app(chrome_identity, &after_policy_cycle.frontmost)
                && after_policy_cycle.pill.visible
                && after_policy_cycle.ring.visible,
            format!(
                "frontmost={:?}, pill={}, ring={}",
                after_policy_cycle.frontmost,
                after_policy_cycle.pill.visible,
                after_policy_cycle.ring.visible
            ),
        ),
        check(
            "panel surfaces captured without Screen Recording",
            panel_snapshots_captured,
            panel_snapshots.join(", "),
        ),
    ];
    let passed = checks.iter().all(|check| check.passed);
    if !passed {
        for check in &mut checks {
            if !check.passed {
                eprintln!("FAIL {}: {}", check.name, check.detail);
            }
        }
    }

    browser.close().await;
    Ok(SpikeReport {
        passed,
        duration_ms: started.elapsed().as_millis(),
        chrome_window_state,
        chrome_viewport,
        before_show,
        after_show,
        after_pill_click,
        quick_entry_open,
        after_quick_entry,
        after_policy_cycle,
        chrome_clicks_through_ring,
        pill_clicks,
        quick_entry_value,
        hide_show_cycle,
        panel_snapshots,
        panel_snapshots_captured,
        checks,
    })
}

#[derive(Clone, Copy)]
enum PanelAction {
    ShowPassive,
    HidePill,
    ShowPill,
    ShowQuickEntry,
    FocusQuickEntry,
    HideQuickEntry,
}

async fn panel_action(app: &AppHandle, action: PanelAction) -> anyhow::Result<()> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let result = (|| -> anyhow::Result<()> {
            match action {
                PanelAction::ShowPassive => {
                    handle
                        .get_webview_panel("ring")
                        .map_err(|_| anyhow::anyhow!("ring panel missing"))?
                        .show();
                    handle
                        .get_webview_panel("pill")
                        .map_err(|_| anyhow::anyhow!("pill panel missing"))?
                        .show();
                }
                PanelAction::HidePill => handle
                    .get_webview_panel("pill")
                    .map_err(|_| anyhow::anyhow!("pill panel missing"))?
                    .hide(),
                PanelAction::ShowPill => handle
                    .get_webview_panel("pill")
                    .map_err(|_| anyhow::anyhow!("pill panel missing"))?
                    .show(),
                PanelAction::ShowQuickEntry => {
                    handle
                        .set_activation_policy(tauri::ActivationPolicy::Accessory)
                        .context("could not enable quick-entry activation")?;
                    handle
                        .get_webview_panel("quick-entry")
                        .map_err(|_| anyhow::anyhow!("quick-entry panel missing"))?
                        .show_and_make_key();
                }
                PanelAction::FocusQuickEntry => {
                    let panel = handle
                        .get_webview_panel("quick-entry")
                        .map_err(|_| anyhow::anyhow!("quick-entry panel missing"))?;
                    panel.make_key_and_order_front();
                    panel.make_key_window();
                }
                PanelAction::HideQuickEntry => {
                    let panel = handle
                        .get_webview_panel("quick-entry")
                        .map_err(|_| anyhow::anyhow!("quick-entry panel missing"))?;
                    panel.resign_key_window();
                    panel.hide();
                    handle
                        .set_activation_policy(tauri::ActivationPolicy::Prohibited)
                        .context("could not restore passive-panel activation policy")?;
                }
            }
            Ok(())
        })();
        let _ = sender.send(result.map_err(|error| error.to_string()));
    })?;
    receiver
        .await
        .context("panel action callback dropped")?
        .map_err(anyhow::Error::msg)
}

async fn capture_panel(app: &AppHandle, label: &str, path: &Path) -> anyhow::Result<()> {
    let window: WebviewWindow = app
        .get_webview_window(label)
        .with_context(|| format!("{label} webview window missing"))?;
    let (sender, receiver) = tokio::sync::oneshot::channel::<Result<Vec<u8>, String>>();
    let sender = Arc::new(Mutex::new(Some(sender)));
    window.with_webview(move |platform| {
        let sender = Arc::clone(&sender);
        let completion = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let result = if image.is_null() {
                if error.is_null() {
                    Err("WKWebView returned neither an image nor an error".to_owned())
                } else {
                    Err("WKWebView snapshot failed".to_owned())
                }
            } else {
                // SAFETY: WebKit owns the NSImage for this callback. TIFF bytes are copied.
                let image = unsafe { &*image };
                image
                    .TIFFRepresentation()
                    .map(|data| data.to_vec())
                    .ok_or_else(|| "NSImage did not provide TIFF data".to_owned())
            };
            if let Some(sender) = sender.lock().expect("snapshot sender poisoned").take() {
                let _ = sender.send(result);
            }
        });
        // SAFETY: Tauri exposes Wry's retained WKWebView subclass through this pointer.
        let webview = unsafe { &*(platform.inner().cast::<WKWebView>()) };
        unsafe {
            webview.takeSnapshotWithConfiguration_completionHandler(None, &completion);
        }
    })?;
    let tiff = receiver
        .await
        .context("WKWebView dropped the panel snapshot callback")?
        .map_err(anyhow::Error::msg)?;
    let image = image::load_from_memory_with_format(&tiff, image::ImageFormat::Tiff)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    image.save_with_format(path, image::ImageFormat::Png)?;
    Ok(())
}
async fn observe(app: &AppHandle) -> anyhow::Result<Observation> {
    let (sender, receiver) = tokio::sync::oneshot::channel();

    let handle = app.clone();
    app.run_on_main_thread(move || {
        let result = (|| -> anyhow::Result<Observation> {
            let pill = handle
                .get_webview_panel("pill")
                .map_err(|_| anyhow::anyhow!("pill panel missing"))?;
            let ring = handle
                .get_webview_panel("ring")
                .map_err(|_| anyhow::anyhow!("ring panel missing"))?;
            let quick_entry = handle
                .get_webview_panel("quick-entry")
                .map_err(|_| anyhow::anyhow!("quick-entry panel missing"))?;
            let frontmost = frontmost_app();
            let mtm = tauri_nspanel::objc2::MainThreadMarker::new()
                .context("observation did not run on the main thread")?;
            let policy = NSApplication::sharedApplication(mtm).activationPolicy();
            Ok(Observation {
                frontmost,
                activation_policy: format!("{policy:?}"),
                pill: panel_facts(pill.as_ref()),
                ring: panel_facts(ring.as_ref()),
                quick_entry: panel_facts(quick_entry.as_ref()),
            })
        })();
        let _ = sender.send(result.map_err(|error| error.to_string()));
    })?;
    receiver
        .await
        .context("panel observation callback dropped")?
        .map_err(anyhow::Error::msg)
}

async fn wait_for_chrome_frontmost(app: &AppHandle) -> anyhow::Result<Observation> {
    for _ in 0..30 {
        let observation = observe(app).await?;
        if observation.frontmost.bundle_id == "com.google.Chrome" {
            return Ok(observation);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let observation = observe(app).await?;
    bail!(
        "Chrome never became frontmost; observed {:?}",
        observation.frontmost
    )
}

fn frontmost_app() -> AppIdentity {
    let application = NSWorkspace::sharedWorkspace().frontmostApplication();
    AppIdentity {
        name: application
            .as_ref()
            .and_then(|app| app.localizedName())
            .map(|name| name.to_string())
            .unwrap_or_default(),
        bundle_id: application
            .as_ref()
            .and_then(|app| app.bundleIdentifier())
            .map(|identifier| identifier.to_string())
            .unwrap_or_default(),
        pid: application
            .as_ref()
            .map(|app| app.processIdentifier())
            .unwrap_or_default(),
    }
}

fn panel_facts(panel: &dyn Panel) -> PanelFacts {
    let native = panel.as_panel();
    let frame = native.frame();
    PanelFacts {
        visible: panel.is_visible(),
        key: native.isKeyWindow(),
        can_become_key: panel.can_become_key_window(),
        can_become_main: panel.can_become_main_window(),
        floating: panel.is_floating_panel(),
        becomes_key_only_if_needed: panel.becomes_key_only_if_needed(),
        hides_on_deactivate: panel.hides_on_deactivate(),
        ignores_mouse_events: native.ignoresMouseEvents(),
        level: native.level() as i64,
        collection_behavior: native.collectionBehavior().bits() as u64,
        style_mask: native.styleMask().bits() as u64,
        frame: [
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ],
    }
}

fn post_click(x: f64, y: f64) -> anyhow::Result<()> {
    for event_type in [
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
    ] {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("could not create HID event source"))?;
        let event =
            CGEvent::new_mouse_event(source, event_type, CGPoint::new(x, y), CGMouseButton::Left)
                .map_err(|_| anyhow::anyhow!("could not create mouse event"))?;
        event.post(CGEventTapLocation::HID);
    }
    Ok(())
}

fn post_text(text: &str) -> anyhow::Result<()> {
    for down in [true, false] {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("could not create HID event source"))?;
        let event = CGEvent::new_keyboard_event(source, 0, down)
            .map_err(|_| anyhow::anyhow!("could not create keyboard event"))?;
        event.set_string(text);
        event.post(CGEventTapLocation::HID);
    }
    Ok(())
}

fn same_app(left: &AppIdentity, right: &AppIdentity) -> bool {
    left.pid != 0 && left.pid == right.pid
}

fn check(name: &'static str, passed: bool, detail: String) -> Check {
    Check {
        name,
        passed,
        detail,
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
