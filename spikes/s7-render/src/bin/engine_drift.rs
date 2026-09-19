use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, ensure};
use neo_cdp::{Browser, LaunchOptions, Page};
use s7_render::fixtures::{Fixture, METRICS_EXPRESSION, engine_drift_fixtures};
use serde::{Deserialize, Serialize};
use serde_json::json;

const DRIFT_LIMIT_PX: f64 = 2.0;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeMetric {
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: String,
    line_height: String,
    display: String,
    scroll_width: f64,
    scroll_height: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureMeasurement {
    id: String,
    patch_confirmed: bool,
    nodes: Vec<NodeMetric>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebKitReport {
    engine: String,
    user_agent: Option<String>,
    device_pixel_ratio: Option<f64>,
    iframe_sandbox: Option<String>,
    fixtures: Vec<FixtureMeasurement>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FieldDrift {
    field: &'static str,
    webkit: f64,
    chrome: f64,
    delta_px: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineDriftLint {
    code: &'static str,
    fixture: String,
    node: String,
    max_drift_px: f64,
    fields: Vec<FieldDrift>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixtureComparison {
    id: String,
    node_count: usize,
    max_drift_px: f64,
    within_limit: bool,
    webkit_patch_confirmed: bool,
    chrome_patch_confirmed: bool,
    missing_in_webkit: Vec<String>,
    missing_in_chrome: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let webkit_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("spikes/out/engine-drift/webkit.json"));
    let out = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("spikes/out/engine-drift"));
    std::fs::create_dir_all(&out)?;

    let webkit: WebKitReport = serde_json::from_slice(
        &std::fs::read(&webkit_path)
            .with_context(|| format!("read WKWebView report {}", webkit_path.display()))?,
    )?;
    ensure!(
        webkit.error.is_none(),
        "WKWebView failed: {:?}",
        webkit.error
    );
    ensure!(
        webkit.fixtures.len() == 5,
        "expected five WKWebView fixtures"
    );

    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(&LaunchOptions::new(profile.path())).await?;
    let page = browser.new_page("about:blank").await?;
    page.set_viewport(1080, 1080, 1.0).await?;

    let mut chrome_fixtures = Vec::new();
    for fixture in engine_drift_fixtures() {
        let measurement = measure_chrome(&page, &fixture).await?;
        let png = page
            .screenshot_png(Some((0.0, 0.0, 1080.0, 1080.0)))
            .await?;
        std::fs::write(out.join(format!("{}-chrome.png", fixture.id)), png)?;
        chrome_fixtures.push(measurement);
    }

    let (comparisons, lints) = compare(&webkit.fixtures, &chrome_fixtures);
    let all_within_limit = comparisons.iter().all(|fixture| fixture.within_limit);
    let same_origin_patching = comparisons
        .iter()
        .all(|fixture| fixture.webkit_patch_confirmed && fixture.chrome_patch_confirmed);
    let max_drift_px = comparisons
        .iter()
        .map(|fixture| fixture.max_drift_px)
        .fold(0.0_f64, f64::max);

    let report = json!({
        "thresholdPx": DRIFT_LIMIT_PX,
        "webkit": {
            "engine": webkit.engine,
            "userAgent": webkit.user_agent,
            "devicePixelRatio": webkit.device_pixel_ratio,
            "iframeSandbox": webkit.iframe_sandbox,
        },
        "chrome": {
            "userAgent": page.evaluate("navigator.userAgent").await?,
            "devicePixelRatio": page.evaluate("devicePixelRatio").await?,
            "protocolCalls": browser.calls(),
        },
        "acceptance": {
            "allFixturesWithin2Px": all_within_limit,
            "sameOriginSandboxPatching": same_origin_patching,
            "maxDriftPx": round(max_drift_px),
            "lintCount": lints.len(),
        },
        "fixtures": comparisons,
        "lints": lints,
    });
    let report_path = out.join("report.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    std::fs::write(
        out.join("chrome.json"),
        serde_json::to_vec_pretty(&chrome_fixtures)?,
    )?;

    println!(
        "engine drift fixtures={} nodes={} max={:.3}px lints={} patching={}",
        chrome_fixtures.len(),
        chrome_fixtures
            .iter()
            .map(|fixture| fixture.nodes.len())
            .sum::<usize>(),
        max_drift_px,
        report["acceptance"]["lintCount"],
        same_origin_patching,
    );
    for fixture in &report["fixtures"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {} nodes={} max={}px pass={}",
            fixture["id"], fixture["nodeCount"], fixture["maxDriftPx"], fixture["withinLimit"]
        );
    }
    println!("report → {}", report_path.display());

    browser.close().await;
    if !same_origin_patching {
        anyhow::bail!("sandboxed iframe patching failed");
    }
    Ok(())
}

async fn measure_chrome(page: &Page, fixture: &Fixture) -> anyhow::Result<FixtureMeasurement> {
    let expression = format!(
        r#"(async()=>{{
            document.open();document.write('<!doctype html><style>*{{box-sizing:border-box}}html,body{{margin:0;width:100%;height:100%;overflow:hidden}}iframe{{width:1080px;height:1080px;border:0}}</style><iframe sandbox="allow-same-origin"></iframe>');document.close();
            const frame=document.querySelector('iframe');
            await new Promise(resolve=>{{frame.onload=resolve;frame.srcdoc={html}}});
            await frame.contentDocument.fonts.ready;
            await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
            const doc=frame.contentDocument,probe=doc.querySelector('[data-n]');
            probe.setAttribute('data-chrome-patched','yes');
            return {{id:{id},patchConfirmed:probe.getAttribute('data-chrome-patched')==='yes',nodes:({metrics})}};
        }})()"#,
        html = serde_json::to_string(fixture.html)?,
        id = serde_json::to_string(fixture.id)?,
        metrics = METRICS_EXPRESSION,
    );
    let value = page.evaluate_async(&expression).await?;
    serde_json::from_value(value).context("decode Chrome fixture metrics")
}

fn compare(
    webkit: &[FixtureMeasurement],
    chrome: &[FixtureMeasurement],
) -> (Vec<FixtureComparison>, Vec<EngineDriftLint>) {
    let webkit: BTreeMap<_, _> = webkit
        .iter()
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect();
    let chrome: BTreeMap<_, _> = chrome
        .iter()
        .map(|fixture| (fixture.id.as_str(), fixture))
        .collect();
    let mut comparisons = Vec::new();
    let mut lints = Vec::new();

    for fixture_id in engine_drift_fixtures().iter().map(|fixture| fixture.id) {
        let webkit_fixture = webkit.get(fixture_id);
        let chrome_fixture = chrome.get(fixture_id);
        let webkit_nodes: BTreeMap<_, _> = webkit_fixture
            .into_iter()
            .flat_map(|fixture| fixture.nodes.iter())
            .map(|node| (node.id.as_str(), node))
            .collect();
        let chrome_nodes: BTreeMap<_, _> = chrome_fixture
            .into_iter()
            .flat_map(|fixture| fixture.nodes.iter())
            .map(|node| (node.id.as_str(), node))
            .collect();
        let missing_in_webkit = chrome_nodes
            .keys()
            .filter(|id| !webkit_nodes.contains_key(**id))
            .map(|id| (*id).to_owned())
            .collect::<Vec<_>>();
        let missing_in_chrome = webkit_nodes
            .keys()
            .filter(|id| !chrome_nodes.contains_key(**id))
            .map(|id| (*id).to_owned())
            .collect::<Vec<_>>();
        let mut max_drift = 0.0_f64;

        for (id, webkit_node) in &webkit_nodes {
            let Some(chrome_node) = chrome_nodes.get(id) else {
                continue;
            };
            let fields = [
                field("x", webkit_node.x, chrome_node.x),
                field("y", webkit_node.y, chrome_node.y),
                field("width", webkit_node.width, chrome_node.width),
                field("height", webkit_node.height, chrome_node.height),
            ];
            let node_max = fields
                .iter()
                .map(|field| field.delta_px)
                .fold(0.0_f64, f64::max);
            max_drift = max_drift.max(node_max);
            let fields = fields
                .into_iter()
                .filter(|field| field.delta_px > DRIFT_LIMIT_PX)
                .collect::<Vec<_>>();
            if !fields.is_empty() {
                lints.push(EngineDriftLint {
                    code: "engine-drift",
                    fixture: fixture_id.to_owned(),
                    node: (*id).to_owned(),
                    max_drift_px: round(node_max),
                    fields,
                });
            }
        }

        let missing = !missing_in_webkit.is_empty() || !missing_in_chrome.is_empty();
        comparisons.push(FixtureComparison {
            id: fixture_id.to_owned(),
            node_count: chrome_nodes.len().max(webkit_nodes.len()),
            max_drift_px: round(max_drift),
            within_limit: !missing && max_drift <= DRIFT_LIMIT_PX,
            webkit_patch_confirmed: webkit_fixture.is_some_and(|fixture| fixture.patch_confirmed),
            chrome_patch_confirmed: chrome_fixture.is_some_and(|fixture| fixture.patch_confirmed),
            missing_in_webkit,
            missing_in_chrome,
        });
    }
    (comparisons, lints)
}

fn field(field: &'static str, webkit: f64, chrome: f64) -> FieldDrift {
    FieldDrift {
        field,
        webkit: round(webkit),
        chrome: round(chrome),
        delta_px: round((webkit - chrome).abs()),
    }
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}
