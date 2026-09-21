#![allow(clippy::expect_used)]
//! The select-all that `replace_text` is built on, driven through a real
//! browser — the only place the platform accelerator can be checked.
//!
//! Skipped when no Chromium-family browser is installed. Everything else is
//! hermetic: a throwaway profile, a `data:` URL, headless.

use std::time::Duration;

use neo_cdp::{Browser, DebugTransport, LaunchOptions};

const FIELD: &str = "data:text/html,<input id=f value=\"old value\">";

#[tokio::test]
async fn replace_text_replaces_rather_than_appends() {
    let Some(chrome) = neo_cdp::chrome_path() else {
        eprintln!("skipped: no Chromium-family browser on this machine");
        return;
    };
    let profile = tempfile::tempdir().expect("temp profile");
    let mut options = LaunchOptions::new(profile.path());
    options.chrome = chrome;
    options.transport = DebugTransport::Pipe;

    let browser = Browser::launch(&options).await.expect("launch");
    let page = browser.new_page(FIELD).await.expect("new page");

    let rect = page
        .evaluate(
            "(() => { const r = document.getElementById('f').getBoundingClientRect(); \
             return [r.left + r.width / 2, r.top + r.height / 2]; })()",
        )
        .await
        .expect("field rect");
    let x = rect[0].as_f64().expect("x");
    let y = rect[1].as_f64().expect("y");
    page.click(x, y).await.expect("click the field");
    page.replace_text("typed").await.expect("replace");

    // The keystroke is delivered to the renderer out of band from the
    // protocol reply, so the value is read back rather than assumed.
    let mut value = String::new();
    for _ in 0..40 {
        value = page
            .evaluate("document.getElementById('f').value")
            .await
            .expect("field value")
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if value == "typed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        value, "typed",
        "select-all did not take: the old value is still there"
    );

    browser.close().await;
}
