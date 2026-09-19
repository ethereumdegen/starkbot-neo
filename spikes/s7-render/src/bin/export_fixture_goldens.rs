use std::path::PathBuf;

use neo_cdp::{Browser, LaunchOptions};
use s7_render::fixtures::engine_drift_fixtures;
use serde_json::json;

const FIXTURE_SIZE: u32 = 1080;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let output_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("spikes/fixtures/golden/chrome"));
    std::fs::create_dir_all(&output_dir)?;

    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(&LaunchOptions::new(profile.path())).await?;
    let page = browser.new_page("about:blank").await?;
    page.set_viewport(FIXTURE_SIZE, FIXTURE_SIZE, 1.0).await?;
    page.call(
        "Emulation.setDefaultBackgroundColorOverride",
        json!({ "color": { "r": 255, "g": 255, "b": 255, "a": 1 } }),
    )
    .await?;

    for fixture in engine_drift_fixtures() {
        let html = serde_json::to_string(fixture.html)?;
        page.evaluate(&format!(
            "document.open();document.write({html});document.close();true"
        ))
        .await?;
        page.evaluate(
            "Promise.all([document.fonts.ready,...[...document.images].map(i=>i.decode().catch(()=>{}))])",
        )
        .await?;
        page.evaluate(
            "document.getAnimations().forEach(a=>{a.pause();const d=Number(a.effect?.getComputedTiming().duration);a.currentTime=Number.isFinite(d)?d:0});true",
        )
        .await?;
        page.evaluate("new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))")
            .await?;
        let png = page
            .screenshot_png(Some((0.0, 0.0, FIXTURE_SIZE as f64, FIXTURE_SIZE as f64)))
            .await?;
        let path = output_dir.join(format!("{}.png", fixture.id));
        std::fs::write(&path, &png)?;
        println!(
            "Chrome golden {:>18}  {:>8} bytes  → {}",
            fixture.id,
            png.len(),
            path.display()
        );
    }

    browser.close().await;
    Ok(())
}
