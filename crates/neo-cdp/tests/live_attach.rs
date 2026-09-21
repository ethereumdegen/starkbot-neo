//! Live Chrome: the attach path, which no fake can prove.
//!
//! `#[ignore]`d because it spawns a real browser (10 §10, 16 §6). Run with
//! `cargo test -p neo-cdp --test live_attach -- --ignored`. It uses a
//! throwaway profile directory and headless windows, so it neither touches
//! the managed profile nor takes the screen; only the transport and the
//! lifetime under test are the real ones.
//!
//! A failed `expect` here is the test failing, which is the point.
#![allow(clippy::expect_used)]

use neo_cdp::{Browser, LaunchOptions, Start};

/// The claim the whole persistent-Chrome change rests on: a browser launched
/// for a profile outlives the handle that started it, and the *next* process
/// to want that profile joins it instead of starting a rival Chrome on the
/// same directory.
#[tokio::test]
#[ignore = "launches a real Chrome"]
async fn a_second_run_joins_the_chrome_the_first_one_left() {
    let profile = tempfile::tempdir().expect("a temporary profile");
    // Headless only so the test does not take over the screen; every other
    // managed-profile default (WebSocket, keep-alive) is what ships.
    let mut options = LaunchOptions::new(profile.path());
    options.headless = true;

    let (first, start) = Browser::attach_or_launch(&options)
        .await
        .expect("chrome starts");
    assert_eq!(
        start,
        Start::Launched,
        "nothing was running on this profile"
    );
    let page = first.new_page("about:blank").await.expect("a tab opens");
    let owned = page.target_id().to_owned();
    // The handle goes away exactly as it does at the end of a run.
    drop(page);
    drop(first);

    let (second, start) = Browser::attach_or_launch(&options)
        .await
        .expect("chrome is still there");
    assert_eq!(
        start,
        Start::Attached,
        "the first run's Chrome must still be running and joinable"
    );
    // The tab the first run left is still there, and is closable by the id
    // the ledger kept — which is what the owned-tab cap does between runs.
    let targets = second
        .call("Target.getTargets", serde_json::json!({}))
        .await
        .expect("targets are listed");
    let ids: Vec<&str> = targets["targetInfos"]
        .as_array()
        .expect("an array of targets")
        .iter()
        .filter_map(|target| target["targetId"].as_str())
        .collect();
    assert!(ids.contains(&owned.as_str()), "the tab survived the run");
    second.close_target(&owned).await.expect("the tab closes");

    second.close().await;
}
