use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use neo_cdp::{Browser, LaunchOptions, Page};
use s7_render::doc::{Author, CanvasDoc, Op};
use serde_json::{Value, json};

const SURFACE: &str = include_str!("../../surface.html");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "spikes/out".into());
    std::fs::create_dir_all(&out)?;
    let profile = tempfile::tempdir()?;
    let browser = Browser::launch(&LaunchOptions::new(profile.path())).await?;
    let page = browser.new_page("about:blank").await?;
    page.set_viewport(1200, 800, 1.0).await?;
    page.evaluate(&format!(
        "document.open();document.write({});document.close();true",
        serde_json::to_string(SURFACE)?
    ))
    .await?;

    let mut doc = CanvasDoc::launch_card();
    set_frame(&page, &doc.frame_html()).await?;
    let started = Instant::now();
    let mut commit_us = Vec::new();

    // Select and drag a real element through the scriptless iframe overlay.
    let headline = screen_rect(&page, "n4").await?;
    click(&page, headline.center()).await?;
    page.evaluate("canvasSpike.drain() ").await?;
    drag(
        &page,
        headline.center(),
        (headline.center().0 + 44.0, headline.center().1 + 22.0),
    )
    .await?;
    let move_intent = take_intent(&page, "move").await?;
    let dx = number(&move_intent, "dx")?;
    let dy = number(&move_intent, "dy")?;
    timed_commit(
        &mut doc,
        &mut commit_us,
        Author::User,
        "drag headline",
        vec![Op::SetStyle {
            node: "n4".into(),
            property: "transform".into(),
            value: Some(format!("translate({dx:.1}px,{dy:.1}px)")),
        }],
    )?;
    apply(&page, json!({"kind":"style","node":"n4","property":"transform","value":format!("translate({dx:.1}px,{dy:.1}px)")})).await?;

    // Resize through the screen-space handle; commit once on pointer release.
    let moved = screen_rect(&page, "n4").await?;
    drag(
        &page,
        (moved.x + moved.w, moved.y + moved.h),
        (moved.x + moved.w + 36.0, moved.y + moved.h + 18.0),
    )
    .await?;
    let resize = take_intent(&page, "resize").await?;
    let width = number(&resize, "w")?;
    let height = number(&resize, "h")?;
    timed_commit(
        &mut doc,
        &mut commit_us,
        Author::User,
        "resize headline",
        vec![
            Op::SetStyle {
                node: "n4".into(),
                property: "width".into(),
                value: Some(format!("{width:.1}px")),
            },
            Op::SetStyle {
                node: "n4".into(),
                property: "min-height".into(),
                value: Some(format!("{height:.1}px")),
            },
        ],
    )?;
    apply(
        &page,
        json!({"kind":"style","node":"n4","property":"width","value":format!("{width:.1}px")}),
    )
    .await?;
    apply(&page, json!({"kind":"style","node":"n4","property":"min-height","value":format!("{height:.1}px")})).await?;

    // A double-click becomes an edit intent; Rust owns the text transaction and patch.
    let moved = screen_rect(&page, "n4").await?;
    double_click(&page, moved.center()).await?;
    let edit = take_intent(&page, "edit").await?;
    ensure!(edit["node"] == "n4", "double-click targeted the wrong node");
    let direct_text = "Radio without the noise.";
    timed_commit(
        &mut doc,
        &mut commit_us,
        Author::User,
        "direct text",
        vec![Op::SetText {
            node: "n4".into(),
            text: direct_text.into(),
        }],
    )?;
    apply(&page, json!({"kind":"text","node":"n4","text":direct_text})).await?;

    // Knob input previews entirely in the surface; only change/commit crosses into Rust.
    let preview = Instant::now();
    page.evaluate("headline.value='144';headline.dispatchEvent(new Event('input',{bubbles:true}));headline.dispatchEvent(new Event('change',{bubbles:true}));true").await?;
    let preview_us = preview.elapsed().as_micros();
    let knob = take_intent(&page, "knob").await?;
    timed_commit(
        &mut doc,
        &mut commit_us,
        Author::User,
        "headline knob",
        vec![Op::KnobSet {
            name: knob["name"].as_str().context("knob name")?.into(),
            value: knob["value"].as_str().context("knob value")?.into(),
        }],
    )?;

    // Pin creation is an overlay intent anchored to a stable data-n id.
    let pin_button = element_rect(&page, "document.getElementById('pin-tool')").await?;
    click(&page, pin_button.center()).await?;
    let headline = screen_rect(&page, "n4").await?;
    click(&page, headline.center()).await?;
    let pin = take_intent(&page, "pin").await?;
    timed_commit(
        &mut doc,
        &mut commit_us,
        Author::User,
        "pin headline",
        vec![Op::PinAdd {
            node: "n4".into(),
            text: pin["text"].as_str().context("pin text")?.into(),
            anchor: (0.5, 0.5),
        }],
    )?;
    let saved_pin = doc.pins.values().next().context("pin was not committed")?;
    apply(&page, json!({"kind":"pin","id":saved_pin.id,"node":saved_pin.node,"text":saved_pin.text,"anchor":saved_pin.anchor})).await?;

    // Pan, cursor-centred zoom, and marquee are driven as pointer/wheel interactions.
    page.evaluate("canvasSpike.setTool('pan');true").await?;
    drag(&page, (80.0, 120.0), (150.0, 160.0)).await?;
    page.evaluate("canvasSpike.setTool('select');true").await?;
    page.call(
        "Input.dispatchMouseEvent",
        json!({"type":"mouseWheel","x":600,"y":400,"deltaX":0,"deltaY":-120,"modifiers":4}),
    )
    .await?;
    drag(&page, (40.0, 700.0), (180.0, 760.0)).await?;
    let marquee = take_intent(&page, "marquee").await?;
    ensure!(
        marquee["rect"]
            .as_array()
            .is_some_and(|rect| rect.len() == 4),
        "marquee did not report a rectangle"
    );

    // Agent subtree rewrite preserves ids. A later user edit wins when the agent pass is undone.
    timed_commit(
        &mut doc,
        &mut commit_us,
        Author::Agent,
        "agent copy pass",
        vec![Op::Rewrite {
            root: "n1".into(),
            changes: vec![
                ("n4".into(), "Charts with signal, not noise.".into()),
                ("n5".into(), "Tune in".into()),
            ],
        }],
    )?;
    set_frame(&page, &doc.frame_html()).await?;
    timed_commit(
        &mut doc,
        &mut commit_us,
        Author::User,
        "user keeps headline",
        vec![Op::SetText {
            node: "n4".into(),
            text: "Radio without the noise.".into(),
        }],
    )?;
    let undo_started = Instant::now();
    let undo_dropped = doc
        .undo(Author::Agent)
        .map_err(anyhow::Error::msg)?
        .dropped
        .clone();
    commit_us.push(undo_started.elapsed().as_micros());
    ensure!(
        undo_dropped == ["node:n4:text"],
        "agent undo did not preserve the later user headline"
    );
    ensure!(
        doc.nodes["n4"].text == "Radio without the noise.",
        "user text was lost"
    );
    ensure!(
        doc.nodes["n5"].text == "Listen now",
        "non-conflicting agent copy did not undo"
    );
    set_frame(&page, &doc.frame_html()).await?;
    let ids = page.evaluate("[...document.querySelector('iframe').contentDocument.querySelectorAll('[data-n]')].map(e=>e.dataset.n)").await?;
    ensure!(
        ids.as_array().is_some_and(|ids| ids.len() == 5),
        "stable node ids were not retained"
    );

    let surface_png = page.screenshot_png(None).await?;
    let surface_path = format!("{out}/hypercanvas-surface.png");
    std::fs::write(&surface_path, surface_png)?;

    // Canonical export uses an isolated Chrome page, not the interactive surface.
    let export = browser.new_page("about:blank").await?;
    export.set_viewport(1080, 1080, 1.0).await?;
    export
        .evaluate(&format!(
            "document.open();document.write({});document.close();true",
            serde_json::to_string(&doc.frame_html())?
        ))
        .await?;
    let export_started = Instant::now();
    let png = export
        .screenshot_png(Some((0.0, 0.0, 1080.0, 1080.0)))
        .await?;
    let export_ms = export_started.elapsed().as_millis();
    let export_path = format!("{out}/hypercanvas-export.png");
    std::fs::write(&export_path, png)?;

    let surface_state = page.evaluate("({panX:canvasSpike.state.panX,panY:canvasSpike.state.panY,zoom:canvasSpike.state.zoom,selected:canvasSpike.state.selected,pins:document.querySelectorAll('.pin').length})").await?;
    let max_commit = commit_us.iter().copied().max().unwrap_or_default();
    println!(
        "hypercanvas interactions PASS in {} ms",
        started.elapsed().as_millis()
    );
    println!(
        "Rust transactions={} rev={} max_commit={} µs agent_undo_dropped={:?}",
        doc.log.len(),
        doc.rev,
        max_commit,
        undo_dropped
    );
    println!(
        "knob preview + browser layout={} µs (0 Rust round trips during input)",
        preview_us
    );
    println!("surface state={surface_state}");
    println!("export 1080x1080={export_ms} ms → {export_path}");
    println!("surface screenshot → {surface_path}");
    println!("CDP protocol calls={}", browser.calls());

    browser.close().await;
    Ok(())
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
impl Rect {
    fn center(self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

async fn set_frame(page: &Page, html: &str) -> anyhow::Result<()> {
    page.evaluate_async(&format!(
        "canvasSpike.setFrame({})",
        serde_json::to_string(html)?
    ))
    .await?;
    Ok(())
}

async fn screen_rect(page: &Page, id: &str) -> anyhow::Result<Rect> {
    let value = page
        .evaluate(&format!(
            "canvasSpike.nodeRect({})",
            serde_json::to_string(id)?
        ))
        .await?;
    let mut rect = rect(value)?;
    rect.y += 48.0;
    Ok(rect)
}

async fn element_rect(page: &Page, expression: &str) -> anyhow::Result<Rect> {
    rect(page.evaluate(&format!("(()=>{{const r=({expression}).getBoundingClientRect();return {{x:r.x,y:r.y,w:r.width,h:r.height}}}})() ")).await?)
}

fn rect(value: Value) -> anyhow::Result<Rect> {
    Ok(Rect {
        x: number(&value, "x")?,
        y: number(&value, "y")?,
        w: number(&value, "w")?,
        h: number(&value, "h")?,
    })
}

fn number(value: &Value, key: &str) -> anyhow::Result<f64> {
    value[key]
        .as_f64()
        .with_context(|| format!("missing numeric {key} in {value}"))
}

async fn click(page: &Page, point: (f64, f64)) -> anyhow::Result<()> {
    page.click(point.0, point.1).await?;
    Ok(())
}

async fn double_click(page: &Page, point: (f64, f64)) -> anyhow::Result<()> {
    for count in [1, 2] {
        for kind in ["mousePressed", "mouseReleased"] {
            page.call(
                "Input.dispatchMouseEvent",
                json!({"type":kind,"x":point.0,"y":point.1,"button":"left","clickCount":count}),
            )
            .await?;
        }
    }
    Ok(())
}

async fn drag(page: &Page, from: (f64, f64), to: (f64, f64)) -> anyhow::Result<()> {
    page.call(
        "Input.dispatchMouseEvent",
        json!({"type":"mouseMoved","x":from.0,"y":from.1}),
    )
    .await?;
    page.call(
        "Input.dispatchMouseEvent",
        json!({"type":"mousePressed","x":from.0,"y":from.1,"button":"left","clickCount":1}),
    )
    .await?;
    for step in 1..=8 {
        let t = step as f64 / 8.0;
        page.call("Input.dispatchMouseEvent", json!({"type":"mouseMoved","x":from.0+(to.0-from.0)*t,"y":from.1+(to.1-from.1)*t,"button":"left"})).await?;
    }
    page.call(
        "Input.dispatchMouseEvent",
        json!({"type":"mouseReleased","x":to.0,"y":to.1,"button":"left","clickCount":1}),
    )
    .await?;
    Ok(())
}

async fn take_intent(page: &Page, expected: &str) -> anyhow::Result<Value> {
    tokio::time::sleep(Duration::from_millis(20)).await;
    let intents = page.evaluate("canvasSpike.drain()").await?;
    let intent = intents
        .as_array()
        .and_then(|values| values.iter().find(|value| value["kind"] == expected))
        .cloned()
        .with_context(|| format!("missing {expected} intent in {intents}"))?;
    Ok(intent)
}

fn timed_commit(
    doc: &mut CanvasDoc,
    timings: &mut Vec<u128>,
    author: Author,
    label: &str,
    ops: Vec<Op>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    doc.commit(author, label, ops).map_err(anyhow::Error::msg)?;
    timings.push(started.elapsed().as_micros());
    Ok(())
}

async fn apply(page: &Page, patch: Value) -> anyhow::Result<()> {
    page.evaluate(&format!("canvasSpike.apply({patch});true"))
        .await?;
    page.evaluate_async("new Promise(resolve=>requestAnimationFrame(()=>resolve(true)))")
        .await?;
    Ok(())
}
