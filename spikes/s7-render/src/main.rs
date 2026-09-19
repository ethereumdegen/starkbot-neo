//! Spike S7: a Hypercanvas frame is HTML + CSS. Can Chrome give us pixel-exact stills at
//! 1×/2×/3× and a deterministic, frame-stepped video of a CSS animation?
//!
//!   s7-render [out_dir]

use std::process::Stdio;
use std::time::Instant;

use neo_cdp::{Browser, LaunchOptions};
use serde_json::json;
use tokio::io::AsyncWriteExt;

const FRAME_HTML: &str = r##"<!doctype html><html><head><meta charset="utf-8"><style>
:root{--bg:#0b1020;--accent:#7c5cff;--ink:#f4f6ff;--pad:72px;--headline:112px;--radius:36px}
*{box-sizing:border-box}html,body{margin:0;background:transparent}
.frame{width:1080px;height:1080px;padding:var(--pad);background:radial-gradient(1200px 700px at 80% 0%,#2a1f6b 0%,var(--bg) 60%);
 color:var(--ink);font-family:-apple-system,"SF Pro Display",system-ui,sans-serif;display:flex;flex-direction:column;
 justify-content:space-between;overflow:hidden;position:relative}
.kicker{font-size:30px;letter-spacing:.18em;text-transform:uppercase;opacity:.75}
h1{font-size:var(--headline);line-height:.98;margin:0;font-weight:800;letter-spacing:-.03em}
h1 em{font-style:normal;color:var(--accent)}
.cta{align-self:flex-start;font-size:38px;font-weight:700;padding:26px 46px;border-radius:var(--radius);background:var(--accent);color:#fff}
.orb{position:absolute;right:-140px;bottom:-140px;width:560px;height:560px;border-radius:50%;
 background:conic-gradient(from 0deg,var(--accent),#22d3ee,#f472b6,var(--accent));filter:blur(8px);opacity:.85;
 animation:spin 2s linear infinite}
.rise{animation:rise 1.2s cubic-bezier(.2,.8,.2,1) both}
@keyframes spin{to{transform:rotate(360deg)}}
@keyframes rise{from{opacity:0;transform:translateY(60px)}to{opacity:1;transform:none}}
</style></head><body>
<div class="frame" data-n="n1"><div class="orb" data-n="n2"></div>
<div class="kicker rise" data-n="n3">Degen Radio · launch week</div>
<h1 class="rise" data-n="n4">Charts that <em>actually</em> slap.</h1>
<div class="cta rise" data-n="n5">Listen now</div></div></body></html>"##;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "spikes/out".into());
    std::fs::create_dir_all(&out)?;
    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(&LaunchOptions::new(profile.path())).await?;
    let page = browser.new_page("about:blank").await?;
    page.call(
        "Emulation.setDefaultBackgroundColorOverride",
        json!({ "color": { "r": 0, "g": 0, "b": 0, "a": 0 } }),
    )
    .await?;
    let html = serde_json::to_string(FRAME_HTML)?;

    // ── stills at three device scales ───────────────────────────────────────────
    for scale in [1.0, 2.0, 3.0] {
        page.set_viewport(1080, 1080, scale).await?;
        page.evaluate(&format!(
            "document.open();document.write({html});document.close();true"
        ))
        .await?;
        // Freeze every animation at its end state so a still is reproducible.
        page.evaluate("document.getAnimations().forEach(a=>{a.pause();a.currentTime=1200});true")
            .await?;
        let timer = Instant::now();
        let png = page
            .screenshot_png(Some((0.0, 0.0, 1080.0, 1080.0)))
            .await?;
        let ms = timer.elapsed().as_millis();
        let again = page
            .screenshot_png(Some((0.0, 0.0, 1080.0, 1080.0)))
            .await?;
        let path = format!("{out}/frame@{scale}x.png");
        std::fs::write(&path, &png)?;
        println!(
            "still @{scale}x  {:>5} ms  {:>8} bytes  identical on re-capture: {}  → {path}",
            ms,
            png.len(),
            png == again
        );
    }

    // ── deterministic frame stepping → ffmpeg ───────────────────────────────────
    let (fps, seconds) = (30u32, 2u32);
    page.set_viewport(1080, 1080, 1.0).await?;
    page.evaluate(&format!(
        "document.open();document.write({html});document.close();true"
    ))
    .await?;
    page.evaluate("document.getAnimations().forEach(a=>a.pause());true")
        .await?;
    let video = format!("{out}/frame.mp4");
    let mut ffmpeg = tokio::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "image2pipe",
            "-framerate",
            &fps.to_string(),
            "-i",
            "-",
        ])
        .args([
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18", &video,
        ])
        .stdin(Stdio::piped())
        .spawn()?;
    let mut stdin = ffmpeg.stdin.take().expect("ffmpeg stdin");
    let timer = Instant::now();
    let mut first_pass = Vec::new();
    for frame in 0..fps * seconds {
        let t = frame as f64 * 1000.0 / fps as f64;
        page.evaluate(&format!(
            "document.getAnimations().forEach(a=>a.currentTime={t});true"
        ))
        .await?;
        let png = page
            .screenshot_png(Some((0.0, 0.0, 1080.0, 1080.0)))
            .await?;
        if frame % 15 == 0 {
            first_pass.push((t, png.clone()));
        }
        stdin.write_all(&png).await?;
    }
    drop(stdin);
    let status = ffmpeg.wait().await?;
    let elapsed = timer.elapsed();
    println!(
        "video  {} frames in {} ms ({:.1} ms/frame, {:.2}× realtime)  ffmpeg ok: {}  → {video}",
        fps * seconds,
        elapsed.as_millis(),
        elapsed.as_millis() as f64 / (fps * seconds) as f64,
        elapsed.as_secs_f64() / seconds as f64,
        status.success()
    );

    // Same timestamps again: is frame stepping deterministic?
    let mut identical = 0;
    for (t, png) in &first_pass {
        page.evaluate(&format!(
            "document.getAnimations().forEach(a=>a.currentTime={t});true"
        ))
        .await?;
        if &page
            .screenshot_png(Some((0.0, 0.0, 1080.0, 1080.0)))
            .await?
            == png
        {
            identical += 1;
        }
    }
    println!(
        "determinism  {identical}/{} re-rendered sample frames byte-identical",
        first_pass.len()
    );

    // A knob is just a CSS variable: how fast is a change visible?
    let timer = Instant::now();
    page.evaluate("document.documentElement.style.setProperty('--headline','140px');document.querySelector('[data-n=n4]').getBoundingClientRect().height").await?;
    println!(
        "knob     CSS-variable change + layout read back in {} µs",
        timer.elapsed().as_micros()
    );

    browser.close().await;
    Ok(())
}
