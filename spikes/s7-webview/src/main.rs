use std::path::PathBuf;

use s7_render::fixtures::{Fixture, METRICS_EXPRESSION, engine_drift_fixtures};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, State};

struct RunState {
    output: PathBuf,
}

#[derive(Serialize)]
struct FixtureBundle {
    fixtures: Vec<Fixture>,
    metrics_expression: &'static str,
}

#[tauri::command]
fn fixture_bundle() -> FixtureBundle {
    FixtureBundle {
        fixtures: engine_drift_fixtures(),
        metrics_expression: METRICS_EXPRESSION,
    }
}

#[tauri::command]
fn report(app: AppHandle, state: State<'_, RunState>, report: Value) -> Result<(), String> {
    if let Some(parent) = state.output.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
    std::fs::write(&state.output, bytes).map_err(|error| error.to_string())?;
    println!("WKWebView measurements → {}", state.output.display());
    app.exit(0);
    Ok(())
}

fn main() {
    let output = std::env::var_os("NEO_WEBKIT_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("spikes/out/engine-drift/webkit.json"));
    tauri::Builder::default()
        .manage(RunState { output })
        .invoke_handler(tauri::generate_handler![fixture_bundle, report])
        .run(tauri::generate_context!())
        .expect("failed to run WKWebView engine-drift spike");
}
