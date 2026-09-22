#![allow(clippy::expect_used)]
//! Child-frame discovery through a real browser. Hidden frames are common
//! analytics/payment embeds; they are not actionable and must not make the
//! containing page unreadable when Chrome has no box model for their owner.

use neo_cdp::{Browser, DebugTransport, LaunchOptions};

const PAGE: &str = "data:text/html,<iframe srcdoc='visible'></iframe><iframe style='display:none' srcdoc='hidden'></iframe>";

#[tokio::test]
async fn hidden_iframe_does_not_abort_frame_discovery() {
    let Some(chrome) = neo_cdp::chrome_path() else {
        eprintln!("skipped: no Chromium-family browser on this machine");
        return;
    };
    let profile = tempfile::tempdir().expect("temp profile");
    let mut options = LaunchOptions::new(profile.path());
    options.chrome = chrome;
    options.transport = DebugTransport::Pipe;

    let browser = Browser::launch(&options).await.expect("launch");
    let page = browser.new_page(PAGE).await.expect("new page");

    let frames = page
        .child_frames()
        .await
        .expect("hidden frames must not abort discovery");
    assert_eq!(frames.len(), 1, "only the visible frame is actionable");

    browser.close().await;
}
