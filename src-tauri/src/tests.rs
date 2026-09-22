//! Bridge tests on Tauri's mock runtime (04 §17).
//!
//! These run the real command bodies against a real `Runtime` on a temporary
//! store. They never touch a subscription port and never write a key: the
//! Keychain in this process is the user's own.

// A failed `expect` in a test is the test failing, which is the point.
#![allow(clippy::expect_used)]

use tauri::Manager;
use tauri::test::{mock_builder, mock_context, noop_assets};

use crate::commands;
use crate::state::Desktop;

fn app(dir: &std::path::Path) -> tauri::App<tauri::test::MockRuntime> {
    let desktop = Desktop::open(dir).expect("the runtime opens on a fresh directory");
    mock_builder()
        .manage(desktop)
        .manage(crate::mode_control::ModeControl::default())
        .build(mock_context(noop_assets()))
        .expect("the mock app builds")
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("neo-desktop-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A fresh install shows both subscription rows signed out, names the three
/// key accounts, and reports inference as not ready — the state the first run
/// has to be able to render.
#[tokio::test]
async fn bootstrap_describes_a_fresh_install() {
    let dir = scratch("bootstrap");
    let app = app(&dir);
    let view = commands::get_bootstrap(app.state())
        .await
        .expect("bootstrap succeeds");

    let providers: Vec<&str> = view
        .connections
        .iter()
        .map(|row| row.provider.as_str())
        .collect();
    assert_eq!(providers, ["anthropic-oauth", "openai-codex"]);
    assert_eq!(view.connections[0].display_name, "Claude Pro/Max");
    assert_eq!(view.connections[1].display_name, "ChatGPT Plus/Pro");

    let accounts: Vec<&str> = view.keys.iter().map(|key| key.account.as_str()).collect();
    assert_eq!(accounts, ["openai", "anthropic", "typesafe"]);
    assert!(
        view.keys
            .iter()
            .any(|key| key.account == "typesafe" && key.required),
        "the TypeSafe key is the one a fresh install cannot do without"
    );
    assert_eq!(view.inference.options.len(), 4);
    assert!(
        view.inference.options.iter().all(|option| !option.usable),
        "nothing is usable before a credential exists"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Deleting a project removes it and its activity from Starkbot, but never
/// removes documents from a folder the user attached.
#[tokio::test]
async fn projects_can_be_deleted_without_deleting_their_files() {
    let dir = scratch("project-delete");
    let root = dir.join("attached");
    std::fs::create_dir_all(&root).expect("the attached project root is created");
    std::fs::write(root.join("soul.md"), "Keep this.").expect("the project document is written");
    let app = app(&dir);
    let created = commands::create_project(
        app.state(),
        "Attached work".to_owned(),
        Some(root.to_string_lossy().into_owned()),
    )
    .await
    .expect("the attached project is created");

    let rows = commands::delete_project(app.state(), created.project.slug)
        .await
        .expect("the project can be deleted");
    assert!(rows.is_empty(), "the deleted project leaves the index");
    assert_eq!(
        std::fs::read_to_string(root.join("soul.md"))
            .expect("the attached project document remains"),
        "Keep this."
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The local project API persists each project's heartbeat cadence independently.
#[tokio::test]
async fn project_heartbeat_interval_is_configurable() {
    let dir = scratch("project-heartbeat-interval");
    let app = app(&dir);
    let created = commands::create_project(app.state(), "Release train".to_owned(), None)
        .await
        .expect("the project is created");

    let configured = commands::configure_project_heartbeat(
        app.state(),
        created.project.slug.clone(),
        true,
        900,
        neo_core::HeartbeatGate::Hold,
    )
    .await
    .expect("the heartbeat interval is configurable");
    assert!(configured.project.heartbeat_enabled);
    assert_eq!(configured.project.heartbeat_every_seconds, 900);

    let shown = commands::show_project(app.state(), created.project.slug)
        .await
        .expect("the configured project can be read back");
    assert_eq!(shown.project.heartbeat_every_seconds, 900);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Every failing doctor row the window can act on carries the control, not a
/// `neo` command line.
#[tokio::test]
async fn doctor_rows_offer_controls_rather_than_shell_commands() {
    let dir = scratch("doctor");
    let app = app(&dir);
    let checks = commands::run_doctor(app.state())
        .await
        .expect("the doctor runs");

    let inference = checks
        .iter()
        .find(|check| check.name == "inference")
        .expect("there is an inference row");
    assert!(
        matches!(inference.fix, Some(crate::view::Fix::ChooseRuntime)),
        "a fresh install's inference row offers the runtime picker"
    );
    for check in &checks {
        if let Some(crate::view::Fix::Manual { detail }) = &check.fix {
            assert!(
                !detail.contains('`'),
                "`{}` handed the user a command to type: {detail}",
                check.name
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Selecting a runtime is what turns `inference: fail` into something the
/// doctor can pass, and it is rejected for anything that is not a K6 path.
#[tokio::test]
async fn selecting_a_runtime_moves_settings_and_refuses_strangers() {
    let dir = scratch("runtime");
    let app = app(&dir);
    let view = commands::set_inference_runtime(
        app.state(),
        "anthropic-oauth".to_owned(),
        Some("sol-latest".to_owned()),
    )
    .await
    .expect("a K6 runtime is selectable");
    assert_eq!(view.provider, "anthropic-oauth");
    assert_eq!(view.model, "sol-latest");
    assert!(
        view.options
            .iter()
            .any(|option| option.provider == "anthropic-oauth" && option.selected)
    );

    let chatgpt = commands::set_inference_runtime(
        app.state(),
        "openai-codex".to_owned(),
        Some("sol-latest".to_owned()),
    )
    .await
    .expect("the ChatGPT runtime is selectable");
    assert_eq!(chatgpt.model, "sol-latest");
    let models = commands::list_models(app.state(), None)
        .await
        .expect("the selected runtime has model choices");
    assert!(
        models.iter().any(|model| model.id == "gpt-5.6-sol"),
        "the model that sol-latest resolves to is available to pin"
    );

    let error = commands::set_inference_runtime(app.state(), "openrouter".to_owned(), None)
        .await
        .expect_err("there is no OpenRouter (K2)");
    assert_eq!(error.code, "unknown_runtime");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Finishing or cancelling a login that was never begun is an error the
/// screen can explain, not a panic — and never a silent success.
#[tokio::test]
async fn login_commands_need_a_handle() {
    let dir = scratch("login");
    let app = app(&dir);
    assert!(
        !commands::cancel_login(app.state(), "openai-codex".to_owned())
            .await
            .expect("cancelling is always answerable"),
        "there is no login to cancel"
    );

    let error = commands::finish_login_pasted(
        app.handle().clone(),
        app.state(),
        "openai-codex".to_owned(),
        "http://localhost:1455/auth/callback?code=whatever&state=whatever".to_owned(),
    )
    .await
    .expect_err("a paste with no login behind it cannot be exchanged");
    assert_eq!(error.code, "no_login");

    let error = commands::begin_login(
        app.handle().clone(),
        app.state(),
        "claude-subscription".to_owned(),
    )
    .await
    .expect_err("only the two OAuth paths are subscription logins");
    assert_eq!(error.code, "unknown_provider");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A key the vendor never saw still answers with a state and nothing else:
/// no value, no echo, no field a token could hide in.
#[tokio::test]
async fn key_rows_carry_state_only() {
    let dir = scratch("keys");
    let app = app(&dir);
    let rows = commands::key_status(app.state())
        .await
        .expect("key status is readable");
    let json = serde_json::to_string(&rows).expect("rows serialise");
    let fields: Vec<&str> = vec![
        "account",
        "label",
        "state",
        "source",
        "required",
        "refreshable",
    ];
    let value: serde_json::Value = serde_json::from_str(&json).expect("rows are objects");
    for row in value.as_array().expect("an array of rows") {
        for key in row.as_object().expect("an object row").keys() {
            assert!(fields.contains(&key.as_str()), "unexpected field `{key}`");
        }
    }

    let error = commands::set_key(app.state(), "not-a-starkbot-key".to_owned(), "x".to_owned())
        .await
        .expect_err("only Starkbot's own accounts are settable");
    assert_eq!(error.code, "unknown_account");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The first frame paints a whole screen from one call: a thread to type
/// into, the switcher's rows, the eval catalogue and the settings a pane
/// edits. A bootstrap missing any of them sends the window straight back for
/// another round trip before it can show anything.
#[tokio::test]
async fn bootstrap_carries_a_whole_screen() {
    let dir = scratch("session");
    let app = app(&dir);
    let view = commands::get_bootstrap(app.state())
        .await
        .expect("bootstrap succeeds");

    assert!(
        view.conversations
            .iter()
            .any(|row| row.id == view.conversation),
        "the thread the window opens in is one of the threads it lists"
    );
    assert!(
        view.messages.is_empty(),
        "a fresh install has said nothing yet"
    );
    assert!(
        view.runs.is_empty(),
        "nothing is running before the window has asked for anything"
    );
    assert!(
        !view.eval_cases.is_empty(),
        "the eval catalogue is compiled in, so it is there before any run"
    );
    let value = serde_json::to_value(&view).expect("the view serialises");
    assert!(
        value["settings"]["identity"].is_object(),
        "the settings pane paints from the bootstrap, not a second call"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Nothing the webview can see may carry a credential. The store holds none
/// — keys live in the Keychain — so the test that matters is that no view
/// model has grown a field one could be put in.
#[tokio::test]
async fn no_view_carries_key_material() {
    const FORBIDDEN: [&str; 8] = [
        "value",
        "secret",
        "token",
        "access_token",
        "refresh_token",
        "api_key",
        "key",
        "password",
    ];
    let dir = scratch("redaction");
    let app = app(&dir);
    let view = commands::get_bootstrap(app.state())
        .await
        .expect("bootstrap succeeds");
    let value = serde_json::to_value(&view).expect("the view serialises");

    let mut queue = vec![("$".to_owned(), value)];
    while let Some((path, node)) = queue.pop() {
        match node {
            serde_json::Value::Object(fields) => {
                for (name, child) in fields {
                    assert!(
                        !FORBIDDEN.contains(&name.as_str()),
                        "`{path}.{name}` is a field a credential could travel in"
                    );
                    queue.push((format!("{path}.{name}"), child));
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.into_iter().enumerate() {
                    queue.push((format!("{path}[{index}]"), child));
                }
            }
            _ => {}
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A turn takes minutes; the command that starts one must not. It answers
/// with the run id the events will carry, and the user's message is already
/// in the thread by the time it does — a window that reloaded a millisecond
/// later must still see what was sent.
#[tokio::test(flavor = "multi_thread")]
async fn send_message_answers_with_a_run_id_at_once() {
    let dir = scratch("send");
    let app = app(&dir);
    let boot = commands::get_bootstrap(app.state())
        .await
        .expect("bootstrap succeeds");

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        commands::send_message(app.state(), boot.conversation, "drive TextEdit".to_owned()),
    )
    .await
    .expect("the command returns without waiting for the turn")
    .expect("the turn starts");

    let thread = commands::load_thread(app.state(), boot.conversation, None)
        .await
        .expect("the thread is readable");
    assert_eq!(
        thread.first().map(|message| message.text.as_str()),
        Some("drive TextEdit"),
        "the user's message is recorded before the turn is spawned"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Steering is answered, never refused.
///
/// The composer stays live while a turn runs, so a message can always be
/// typed a moment after the turn it was meant for ended. That race is the
/// normal case, not an error: the command says `false` and the window sends
/// what was typed as a new turn instead of losing it behind a banner.
#[tokio::test]
async fn steering_a_run_that_ended_is_answered_not_refused() {
    let dir = scratch("steer");
    let app = app(&dir);

    let taken = commands::steer_run(app.state(), neo_core::RunId::new(), "left a bit".to_owned())
        .await
        .expect("steering is always answerable");
    assert!(!taken, "no run by that id is still taking messages");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Stop has to actually stop. The registry is what makes it real: the token
/// the work was given is the token the button cancels, and a run nobody
/// started answers `false` rather than pretending.
#[tokio::test]
async fn stop_run_cancels_the_token_the_work_holds() {
    let dir = scratch("stop");
    let app = app(&dir);
    let runs = app.state::<Desktop>().runs();

    let run = neo_core::RunId::new();
    let cancel = runs
        .start(run, crate::view::RunKind::Nav)
        .expect("a nav run is not exclusive");
    assert!(!cancel.is_cancelled());

    let view = commands::get_bootstrap(app.state())
        .await
        .expect("bootstrap succeeds");
    assert_eq!(
        view.runs.len(),
        1,
        "a reloaded window is told what is still running"
    );

    assert!(
        commands::stop_run(app.state(), run)
            .await
            .expect("stopping is always answerable")
    );
    assert!(
        cancel.is_cancelled(),
        "the work's own token is what was cancelled"
    );
    assert!(
        !commands::stop_run(app.state(), neo_core::RunId::new())
            .await
            .expect("stopping is always answerable"),
        "there was no such run to stop"
    );

    // The entry survives the cancellation: a run closing its browser is still
    // a run, and only the task that owns it may forget it.
    runs.finish(run);
    assert!(runs.snapshot().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Two suites at once would type into each other: the cases share the
/// keyboard and the frontmost window, which is why `neo_eval` runs at
/// `concurrency: 1`. The refusal has to come from the bridge, before
/// anything is launched.
#[tokio::test]
async fn a_second_eval_is_refused_while_one_is_running() {
    let dir = scratch("eval");
    let app = app(&dir);
    let runs = app.state::<Desktop>().runs();
    let running = neo_core::RunId::new();
    assert!(
        runs.start(running, crate::view::RunKind::Eval).is_some(),
        "the first suite starts"
    );

    let error = commands::run_eval(app.state(), None, None, true)
        .await
        .expect_err("the second suite is refused");
    assert_eq!(error.code, "eval_busy");
    assert_eq!(
        runs.snapshot().len(),
        1,
        "the refused run left nothing behind in the registry"
    );

    // A suite that has ended releases the exclusion rather than wedging the
    // screen until the app is restarted.
    runs.finish(running);
    assert!(
        runs.start(neo_core::RunId::new(), crate::view::RunKind::Eval)
            .is_some()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A subscriber that fell behind is told so in the stream it is already
/// reading, because `Lagged` never reaches JavaScript and a silent gap is a
/// thread the window renders wrong.
#[test]
fn a_dropped_event_is_announced_as_a_notice() {
    let envelope = crate::events::gap(41, 7);
    let value = serde_json::to_value(&envelope).expect("the envelope serialises");
    assert_eq!(value["seq"], 41, "the gap does not invent a new sequence");
    assert_eq!(value["event"]["type"], "notice");
    assert_eq!(value["event"]["code"], crate::events::GAP);
    assert!(
        value["event"]["text"]
            .as_str()
            .is_some_and(|text| text.contains('7')),
        "the notice says how much was missed"
    );
}

/// Threads are what the window is organised around: it can start one, name
/// it, find it again by the id it was given, and close it — after which the
/// switcher no longer lists it, and closing it again is not an error.
#[tokio::test]
async fn conversations_can_be_started_renamed_and_closed() {
    let dir = scratch("threads");
    let app = app(&dir);
    let created = commands::new_conversation(app.state(), Some("nav work".to_owned()))
        .await
        .expect("a thread is created");
    assert_eq!(created.title.as_deref(), Some("nav work"));

    commands::rename_conversation(app.state(), created.id, "nav work, day two".to_owned())
        .await
        .expect("a thread that exists can be renamed");

    let rows = commands::list_conversations(app.state(), None)
        .await
        .expect("threads are listable");
    let row = rows
        .iter()
        .find(|row| row.id == created.id)
        .expect("the new thread is in the switcher");
    assert_eq!(row.title.as_deref(), Some("nav work, day two"));

    commands::delete_conversation(app.state(), created.id)
        .await
        .expect("a thread can be closed");
    let rows = commands::list_conversations(app.state(), None)
        .await
        .expect("threads are listable");
    assert!(
        rows.iter().all(|row| row.id != created.id),
        "a closed thread leaves the switcher"
    );
    commands::delete_conversation(app.state(), created.id)
        .await
        .expect("closing a thread that is already gone is fine");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings are edited section by section, and the store's answer — not the
/// pane's own copy — is what the window paints next.
#[tokio::test]
async fn patching_a_section_answers_with_the_merged_settings() {
    let dir = scratch("settings");
    let app = app(&dir);
    let patched = commands::patch_settings(
        app.state(),
        "identity".to_owned(),
        serde_json::json!({ "name": "Ada" }),
    )
    .await
    .expect("a valid patch lands");
    let value = serde_json::to_value(&patched).expect("settings serialise");
    assert_eq!(value["identity"]["name"], "Ada");

    let error = commands::patch_settings(
        app.state(),
        "identity".to_owned(),
        serde_json::json!({ "name": "" }),
    )
    .await
    .expect_err("an empty name is not a name");
    assert_eq!(error.code, "validation");

    let current = commands::get_settings(app.state())
        .await
        .expect("settings are readable");
    let value = serde_json::to_value(&current).expect("settings serialise");
    assert_eq!(
        value["identity"]["name"], "Ada",
        "the rejected patch changed nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole point of the bridge: what the core publishes reaches the
/// webview, on one channel, with the sequence number a front end detects a
/// gap by. A window that has to poll for progress is a window that shows a
/// spinner instead of the work.
#[tokio::test(flavor = "multi_thread")]
async fn published_events_reach_the_webview_on_one_channel() {
    use tauri::Listener;

    let dir = scratch("bridge");
    let app = app(&dir);
    let runtime = app.state::<Desktop>().runtime();
    crate::events::forward(app.handle().clone(), &runtime);

    let (sender, received) = std::sync::mpsc::channel();
    app.listen(crate::events::CHANNEL, move |event| {
        let _ = sender.send(event.payload().to_owned());
    });

    let conversation = neo_core::ConversationId::new();
    runtime.publish(neo_core::AppEvent::ConversationReset {
        conversation_id: conversation,
    });

    let payload = received
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the event reaches the window");
    let value: serde_json::Value =
        serde_json::from_str(&payload).expect("the payload is an envelope");
    assert!(
        value["seq"].as_u64().is_some(),
        "a front end cannot detect a gap without the sequence"
    );
    assert_eq!(
        value["event"]["type"], "conversation_reset",
        "the core's own vocabulary crosses, not a second one"
    );
    assert_eq!(value["event"]["conversation_id"], conversation.to_string());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A webview built against another protocol is turned away in words, with
/// both versions and the command that repairs it — not left to discover the
/// mismatch by reading a field that is no longer there.
#[test]
fn a_stale_webview_is_refused_by_name() {
    assert_eq!(
        commands::handshake(neo_agent::BRIDGE_VERSION).expect("the built-in UI matches"),
        neo_agent::BRIDGE_VERSION
    );

    let error = commands::handshake(neo_agent::BRIDGE_VERSION + 1)
        .expect_err("a UI from another protocol is refused");
    assert_eq!(error.code, "bridge_version");
    assert!(
        error
            .message
            .contains(&format!("v{}", neo_agent::BRIDGE_VERSION + 1))
            && error
                .message
                .contains(&format!("v{}", neo_agent::BRIDGE_VERSION)),
        "both versions belong in the message: {}",
        error.message
    );
    match error.fix {
        Some(crate::view::Fix::Manual { detail }) => {
            assert!(
                detail.contains("npm run build"),
                "the fix names it: {detail}"
            );
        }
        other => panic!("a mismatch is fixed by rebuilding the UI, not by {other:?}"),
    }
}

/// The whole reason the control socket exists: a message handed to a shell
/// has to land in the window somebody is watching. What proves it landed is
/// the store — a thread that exists, named after what was said, with the
/// user's own line already in it — because that is what the open window
/// re-reads when the turn it started begins to publish.
#[tokio::test(flavor = "multi_thread")]
async fn a_control_message_becomes_a_thread_with_the_user_in_it() {
    let dir = scratch("control");
    let app = app(&dir);
    let desktop = app.state::<Desktop>();

    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::control::handle(
            r#"{"say":"make a cool logo for starkbot"}"#,
            desktop.runtime(),
            desktop.runs(),
            app.handle().clone(),
        ),
    )
    .await
    .expect("the handler answers without waiting for the turn");
    let answer = serde_json::to_value(&answer).expect("the response is JSON");
    assert_eq!(
        answer.get("ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "the request was accepted: {answer}"
    );
    let conversation: neo_core::ConversationId = answer
        .get("conversation")
        .and_then(serde_json::Value::as_str)
        .expect("a conversation id comes back")
        .parse()
        .expect("the id is a uuid");
    assert!(
        answer
            .get("run")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "the run id the events will carry comes back too: {answer}"
    );

    let thread = commands::load_thread(app.state(), conversation, None)
        .await
        .expect("the new thread is readable");
    assert_eq!(
        thread.first().map(|message| message.text.as_str()),
        Some("make a cool logo for starkbot"),
        "the message is in the thread before the turn is spawned"
    );

    let rows = commands::list_conversations(app.state(), None)
        .await
        .expect("threads are listable");
    let row = rows
        .iter()
        .find(|row| row.id == conversation)
        .expect("the new thread is in the switcher");
    assert_eq!(
        row.title.as_deref(),
        Some("make a cool logo for starkbot"),
        "an untitled request names its thread after what was asked"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Malformed JSON, invalid modes and ambiguous actions cannot create a turn.
#[tokio::test]
async fn a_malformed_control_line_is_answered_not_fatal() {
    let dir = scratch("control-junk");
    let app = app(&dir);
    let desktop = app.state::<Desktop>();

    for line in [
        "{ not json",
        r#"{"window_mode":"unknown"}"#,
        r#"{"say":"do not send this","window_mode":"mini"}"#,
    ] {
        let answer = crate::control::handle(
            line,
            desktop.runtime(),
            desktop.runs(),
            app.handle().clone(),
        )
        .await;
        let answer = serde_json::to_value(&answer).expect("the response is JSON");
        assert_eq!(
            answer.get("ok").and_then(serde_json::Value::as_bool),
            Some(false)
        );
    }
    let rows = commands::list_conversations(app.state(), None)
        .await
        .expect("threads are listable");
    assert!(
        rows.is_empty(),
        "a rejected request must not create a conversation"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The socket itself, which is the only part a shell ever touches: a file
/// nobody else on the machine may open, taken back from a crashed window's
/// leftovers, answering one line with one line.
#[tokio::test(flavor = "multi_thread")]
async fn the_control_socket_answers_a_line_and_is_private() {
    use std::os::unix::fs::PermissionsExt;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let dir = scratch("control-socket");
    let app = app(&dir);
    let desktop = app.state::<Desktop>();
    let socket = neo_core::paths::control_socket(&dir);
    // What a crash leaves behind. The next window has to take it back, or
    // `neo say` would report no window while one is on screen.
    std::fs::write(&socket, b"a crashed window's leftovers").expect("a stale socket file");

    let serving = tokio::spawn(crate::control::serve(
        socket.clone(),
        desktop.runtime(),
        desktop.runs(),
        app.handle().clone(),
    ));
    let mut stream = connect(&socket).await;

    let mode = std::fs::metadata(&socket)
        .expect("the socket exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "this is a door into the user's agent, so only the user may open it"
    );

    stream
        .write_all(b"{\"say\":\"drive TextEdit\",\"title\":\"from a shell\"}\n")
        .await
        .expect("the request is written");
    let mut answer = String::new();
    tokio::io::BufReader::new(stream)
        .read_line(&mut answer)
        .await
        .expect("one line comes back");
    let answer: serde_json::Value =
        serde_json::from_str(answer.trim()).expect("the answer is JSON");
    assert_eq!(
        answer.get("ok").and_then(serde_json::Value::as_bool),
        Some(true),
        "the request was accepted: {answer}"
    );

    let conversation: neo_core::ConversationId = answer
        .get("conversation")
        .and_then(serde_json::Value::as_str)
        .expect("a conversation id comes back")
        .parse()
        .expect("the id is a uuid");
    let rows = commands::list_conversations(app.state(), None)
        .await
        .expect("threads are listable");
    let row = rows
        .iter()
        .find(|row| row.id == conversation)
        .expect("the thread the socket started is in the switcher");
    assert_eq!(
        row.title.as_deref(),
        Some("from a shell"),
        "a request that named its thread gets that name"
    );

    serving.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Binding happens on the listener's own task, so a caller that connects the
/// instant after it is spawned can legitimately arrive first.
async fn connect(socket: &std::path::Path) -> tokio::net::UnixStream {
    for _ in 0..100u32 {
        if let Ok(stream) = tokio::net::UnixStream::connect(socket).await {
            return stream;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the control socket never came up at {}", socket.display())
}
