//! The review set's plumbing, against a real Chrome (16 §6.2).
//!
//! `#[ignore]`d because it launches a browser, like `neo-cdp`'s live attach
//! test. Run with
//! `cargo test -p neo-eval --test live_fixtures -- --ignored`.
//!
//! What it proves is the claim every navigation case rests on and no unit test
//! can: a fixture page records what was done to it, and the probe reads that
//! record back **from a different tab** — which is the only way to score a
//! page the harness cannot attach to. It uses a throwaway data directory and
//! headless windows, so it touches neither the managed profile nor the screen.
//!
//! A failed `expect` here is the test failing, which is the point.
#![allow(clippy::expect_used)]

use std::path::Path;

use neo_cdp::{Browser, LaunchOptions, Page};
use neo_eval::pages;

/// A tab on the same profile the probe reads, driven by this test rather than
/// by the navigator: what is under test is the fixture and the read-back, not
/// `jev-nav`.
async fn tab(data_dir: &Path, url: &str) -> (Browser, Page) {
    let mut options = LaunchOptions::new(pages::profile(data_dir));
    options.headless = true;
    let (browser, _start) = Browser::attach_or_launch(&options)
        .await
        .expect("chrome starts on the throwaway profile");
    let page = browser.new_page(url).await.expect("a tab opens");
    (browser, page)
}

/// Every review-set page, as the navigator is offered it.
///
/// This is the other half of "the case is runnable": a case whose target
/// control is never in the action space cannot pass however good the model is,
/// and that is a fixture bug, not a model result. It runs the same
/// `CdpObserver` a browser run drives, so the answer is the real action space
/// and not a query this test invented.
#[tokio::test]
#[ignore = "launches a real Chrome"]
async fn every_case_target_is_in_the_action_space() {
    let data = tempfile::tempdir().expect("a temporary data directory");
    pages::serve().await.expect("the fixture server starts");
    pages::reset_state(data.path(), &pages::state_url())
        .await
        .expect("the record clears");

    // (page, action kind, a fragment of the label the case needs)
    let wanted = [
        ("nav-form.html", "fill", "Full name"),
        ("nav-form.html", "click", "Save request"),
        ("nav-crossorigin.html", "click", "Acknowledge notice 8841"),
        ("nav-shadow.html", "fill", "Display name"),
        ("nav-shadow.html", "click", "Store display name"),
        ("nav-popup.html", "click", "Open receipt 5120"),
        ("nav-scroll.html", "scroll", "Warehouse rows"),
        ("nav-select.html", "select", "Seat plan"),
        ("nav-select.html", "click", "Store plan choice"),
        ("nav-keys.html", "press", "ArrowDown"),
        ("nav-autocomplete.html", "fill", "Destination"),
        ("nav-search.html", "fill", "City"),
        ("nav-search.html", "click", "Run search"),
        ("nav-animation.html", "click", "Refresh status"),
        ("nav-pay.html", "click", "Pay 12.00 EUR"),
        ("nav-nonweb.html", "click", "Reach the delivery desk"),
    ];

    for (page, kind, label) in wanted {
        let (_browser, tab) = tab(data.path(), &pages::url(page)).await;
        tab.set_viewport(1120, 780, 1.0).await.expect("a viewport");
        let mut observer = jev_nav::web::CdpObserver::new(tab.clone());
        let observation = observer.observe().await.expect("an observation");
        let offered: Vec<String> = observation["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|action| action["kind"] == kind)
            .filter_map(|action| action["label"].as_str().map(str::to_owned))
            .collect();
        assert!(
            offered.iter().any(|offer| offer.contains(label)),
            "{page} offers no {kind} action containing {label:?}; it offers {offered:?}"
        );
        tab.close().await.expect("the tab closes");
    }
}

/// The three page-level facts the rules layer acts on, as `snapshot.js`
/// reports them: the login wall is a hand-over, the non-web link is a refusal
/// the gate can see, and an upload is not even offered while the run carries
/// no attachment (which is why the upload case is a known gap).
#[tokio::test]
#[ignore = "launches a real Chrome"]
async fn the_pages_carry_the_signals_the_rules_layer_reads() {
    let data = tempfile::tempdir().expect("a temporary data directory");
    pages::serve().await.expect("the fixture server starts");

    let (_browser, wall) = tab(data.path(), &pages::url("nav-login.html")).await;
    pages::reset_state(data.path(), &pages::state_url())
        .await
        .expect("the record clears");
    wall.navigate(&pages::url("nav-login.html"))
        .await
        .expect("the wall reloads with no session");
    let mut observer = jev_nav::web::CdpObserver::new(wall.clone());
    let observation = observer.observe().await.expect("an observation");
    assert_eq!(
        observation["signals"]["password_fields"],
        serde_json::json!(1),
        "the login fixture must read as a sign-in wall"
    );
    wall.close().await.expect("the tab closes");

    let (_browser, desk) = tab(data.path(), &pages::url("nav-nonweb.html")).await;
    let mut observer = jev_nav::web::CdpObserver::new(desk.clone());
    let observation = observer.observe().await.expect("an observation");
    let href = observation["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|action| action["href"].as_str().map(str::to_owned))
        .expect("the desk link is offered with its destination");
    assert!(href.starts_with("mailto:"), "the only way onward is {href}");
    desk.close().await.expect("the tab closes");

    let (_browser, intake) = tab(data.path(), &pages::url("nav-upload.html")).await;
    let mut observer = jev_nav::web::CdpObserver::new(intake.clone());
    let observation = observer.observe().await.expect("an observation");
    let uploads = observation["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|action| action["kind"] == "upload")
        .count();
    assert_eq!(
        uploads, 0,
        "an agent turn attaches nothing, so no upload action may be offered — \
         when this starts failing, `nav-upload-confirm` can stop being a known gap"
    );
    // With an attachment the same page does offer it, which is what makes the
    // gap a wiring gap and not a fixture bug.
    let mut observer = jev_nav::web::CdpObserver::new(intake.clone()).with_attachments(vec![
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../spikes/fixtures/upload.txt"),
    ]);
    let observation = observer.observe().await.expect("an observation");
    let uploads = observation["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|action| action["kind"] == "upload")
        .count();
    assert_eq!(uploads, 1, "the asset field is an upload target");
    intake.close().await.expect("the tab closes");
}

#[tokio::test]
#[ignore = "launches a real Chrome"]
async fn a_page_records_what_was_done_and_the_probe_reads_it_back() {
    let data = tempfile::tempdir().expect("a temporary data directory");
    pages::serve().await.expect("the fixture server starts");

    // The fixture step: whatever an earlier case left is gone.
    let cleared = pages::reset_state(data.path(), &pages::state_url())
        .await
        .expect("the record clears");
    assert_eq!(cleared["cleared"], serde_json::json!(true));
    let empty = pages::read_state(data.path(), &pages::state_url())
        .await
        .expect("the record reads");
    assert_eq!(empty["state"], serde_json::json!({}));

    // The run: a form filled and saved. Driven here through the page's own
    // events, because the assertion is about what the *page* records.
    let (_browser, page) = tab(data.path(), &pages::url("nav-form.html")).await;
    page.evaluate(
        "(() => { const q = id => document.getElementById(id);
          q('name').value = 'Ada Lovelace';
          q('email').value = 'ada@starkbot.test';
          q('f').dispatchEvent(new Event('submit', { cancelable: true }));
          return true; })()",
    )
    .await
    .expect("the form saves");
    page.close().await.expect("the driven tab closes");

    // The probe: a different tab, after the driven one is gone.
    let observed = pages::read_state(data.path(), &pages::state_url())
        .await
        .expect("the record reads");
    assert_eq!(observed["state"]["request"]["name"], "Ada Lovelace");
    assert_eq!(observed["state"]["request"]["email"], "ada@starkbot.test");
    assert_eq!(observed["session"], serde_json::json!(false));
}

/// The cross-origin case's premise: the panel really is a separate origin, and
/// the only record of the click comes from inside it.
#[tokio::test]
#[ignore = "launches a real Chrome"]
async fn the_embedded_panel_is_cross_origin_and_reports_through_its_parent() {
    let data = tempfile::tempdir().expect("a temporary data directory");
    pages::serve().await.expect("the fixture server starts");
    pages::reset_state(data.path(), &pages::state_url())
        .await
        .expect("the record clears");

    let (browser, page) = tab(data.path(), &pages::url("nav-crossorigin.html")).await;
    // Chrome isolates a cross-site frame into its own target; that is what the
    // observer has to reach through a per-frame isolated world.
    let targets = browser
        .call("Target.getTargets", serde_json::json!({}))
        .await
        .expect("the target list");
    assert!(
        targets["targetInfos"].as_array().is_some_and(|targets| {
            targets.iter().any(|target| {
                target["type"] == "iframe"
                    && target["url"]
                        .as_str()
                        .is_some_and(|url| url.contains(pages::ALT_HOST))
            })
        }),
        "the panel was not isolated as its own target: {targets}"
    );

    // Clicking inside the frame, from inside the frame.
    let frame = page.child_frames().await.expect("the frame list");
    let context = frame
        .first()
        .map(|child| child.id.clone())
        .expect("the panel's frame");
    let world = page
        .create_isolated_world_for_frame(&context, "eval-smoke")
        .await
        .expect("an isolated world in the panel");
    page.evaluate_in_context(world, "document.getElementById('ack').click(); true", false)
        .await
        .expect("the panel's button clicks");
    page.close().await.expect("the driven tab closes");

    let observed = pages::read_state(data.path(), &pages::state_url())
        .await
        .expect("the record reads");
    assert_eq!(observed["state"]["notice"]["state"], "acknowledged");
    assert_eq!(observed["state"]["notice"]["id"], serde_json::json!(8841));
}

/// The login-wall case's premise: the harness can do the human part from its
/// own tab, and a tab that is already open on the wall sees it come down —
/// which is what a resumed run re-observes.
#[tokio::test]
#[ignore = "launches a real Chrome"]
async fn opening_the_session_takes_the_wall_down_in_a_tab_that_is_already_open() {
    let data = tempfile::tempdir().expect("a temporary data directory");
    pages::serve().await.expect("the fixture server starts");
    pages::reset_state(data.path(), &pages::state_url())
        .await
        .expect("the record clears");

    let (_browser, page) = tab(data.path(), &pages::url("nav-login.html")).await;
    let walled = page
        .evaluate("document.querySelectorAll('input[type=password]').length")
        .await
        .expect("the wall is legible");
    assert_eq!(walled, serde_json::json!(1), "the wall must be up first");

    // The person signs in. A different tab, exactly as the card path does it.
    pages::open_session(data.path(), &pages::state_url())
        .await
        .expect("the session opens");

    // `storage` is asynchronous across tabs, so give the open tab a moment to
    // hear about it — a resumed run re-observes, which takes far longer.
    let mut inside = serde_json::json!(false);
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        inside = page
            .evaluate("!document.getElementById('inside').hidden")
            .await
            .expect("the page is legible");
        if inside == serde_json::json!(true) {
            break;
        }
    }
    assert_eq!(
        inside,
        serde_json::json!(true),
        "the wall did not come down in the tab that was already open"
    );

    // And the work behind it can then be done and recorded.
    page.evaluate("document.getElementById('seen').click(); true")
        .await
        .expect("the invoice is marked");
    page.close().await.expect("the driven tab closes");
    let observed = pages::read_state(data.path(), &pages::state_url())
        .await
        .expect("the record reads");
    assert_eq!(observed["state"]["seen"]["invoice"], "INV-4417");
    assert_eq!(observed["session"], serde_json::json!(true));
}
