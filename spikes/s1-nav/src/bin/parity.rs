use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, ensure};
use jev_nav::policy::Action;
use jev_nav::web::CdpObserver;
use neo_cdp::{Browser, DebugTransport, LaunchOptions};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_fixtures(
    root: PathBuf,
    frame_origin: Option<String>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let root = root.clone();
            let frame_origin = frame_origin.clone();
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let Ok(size) = socket.read(&mut request).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&request[..size]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|path| path.split('?').next())
                    .unwrap_or("/");
                let filename = match path {
                    "/" | "/browser-parity.html" => "browser-parity.html",
                    "/frame.html" => "frame.html",
                    "/popup.html" => "popup.html",
                    _ => "",
                };
                let (status, body) = if filename.is_empty() {
                    ("404 Not Found", b"not found".to_vec())
                } else {
                    match tokio::fs::read(root.join(filename)).await {
                        Ok(body) if filename == "browser-parity.html" && frame_origin.is_some() => {
                            let body = String::from_utf8_lossy(&body).replace(
                                "src=\"frame.html\"",
                                &format!("src=\"{}/frame.html\"", frame_origin.as_deref().unwrap()),
                            );
                            ("200 OK", body.into_bytes())
                        }
                        Ok(body) => ("200 OK", body),
                        Err(_) => ("404 Not Found", b"not found".to_vec()),
                    }
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    Ok((format!("http://{address}"), task))
}

fn find_action(observation: &Value, kind: &str, label: &str) -> anyhow::Result<Action> {
    observation["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|action| {
            action["kind"] == kind
                && action["label"]
                    .as_str()
                    .is_some_and(|candidate| candidate.contains(label))
        })
        .and_then(Value::as_object)
        .cloned()
        .with_context(|| format!("missing {kind} action containing {label:?}"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures")
        .canonicalize()?;
    let upload = fixture_dir.join("upload.txt").canonicalize()?;
    let (frame_origin, frame_server) = serve_fixtures(fixture_dir.clone(), None).await?;
    let frame_origin = frame_origin.replacen("127.0.0.1", "localhost", 1);
    let (base_url, fixture_server) =
        serve_fixtures(fixture_dir.clone(), Some(frame_origin)).await?;
    let profile = tempfile::tempdir()?;
    let transport = if std::env::args().any(|argument| argument == "--websocket") {
        DebugTransport::WebSocket
    } else {
        DebugTransport::Pipe
    };
    let transport_name = match transport {
        DebugTransport::Pipe => "pipe",
        DebugTransport::WebSocket => "websocket",
    };
    let started = Instant::now();
    let mut options = LaunchOptions::new(profile.path());
    options.transport = transport;
    let browser = Browser::launch(&options).await?;
    let page = browser
        .new_page(&format!("{base_url}/browser-parity.html"))
        .await?;
    page.set_viewport(1120, 780, 1.0).await?;
    let targets = browser
        .call("Target.getTargets", serde_json::json!({}))
        .await?;
    ensure!(
        targets["targetInfos"].as_array().is_some_and(|targets| {
            targets.iter().any(|target| {
                target["type"] == "iframe"
                    && target["url"]
                        .as_str()
                        .is_some_and(|url| url.starts_with("http://localhost:"))
            })
        }),
        "cross-site frame was not isolated as an OOPIF target"
    );
    let mut observer = CdpObserver::new(page).with_attachments(vec![upload]);

    let mut observation = observer.observe().await?;
    ensure!(
        observation["signals"]["cross_origin_frames"] == 0,
        "isolated world could not observe the cross-origin fixture frame"
    );

    let action = find_action(&observation, "fill", "Shadow name")?;
    observer.act(&action, &observation, Some("Ada")).await?;
    observation = observer.observe().await?;
    ensure!(observer.page().evaluate("document.querySelector('shadow-form').shadowRoot.getElementById('shadow-name').value").await? == "Ada", "shadow-root typing failed");

    let action = find_action(&observation, "click", "Run frame action")?;
    observer.act(&action, &observation, None).await?;
    observation = observer.observe().await?;
    ensure!(
        observation["text"]
            .as_str()
            .is_some_and(|text| text.contains("clicked")),
        "cross-origin iframe click failed"
    );

    let action = find_action(&observation, "fill", "Draft message")?;
    observer
        .act(&action, &observation, Some("First line\nSecond line"))
        .await?;
    observation = observer.observe().await?;
    ensure!(
        observer
            .page()
            .evaluate("document.querySelector('.editor').innerText.replace(/\\s+/g,' ').trim()")
            .await?
            == "First line Second line",
        "contenteditable read-back failed"
    );

    let action = find_action(&observation, "upload", "Campaign asset")?;
    observer.act(&action, &observation, None).await?;
    observation = observer.observe().await?;
    ensure!(
        observer
            .page()
            .evaluate("document.getElementById('asset').files[0]?.name")
            .await?
            == "upload.txt",
        "file upload failed"
    );

    let action = find_action(&observation, "scroll", "Result list")?;
    observer.act(&action, &observation, None).await?;
    observer.observe().await?;
    ensure!(
        observer
            .page()
            .evaluate("document.querySelector('.scroller').scrollTop > 0")
            .await?
            == true,
        "nested scroll failed"
    );

    observer
        .page()
        .evaluate("document.getElementById('keys').focus(); true")
        .await?;
    observation = observer.observe().await?;
    let action = observation["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|action| action["kind"] == "press" && action["key"] == "ArrowDown")
        .and_then(Value::as_object)
        .cloned()
        .context("missing focused ArrowDown action")?;
    observer.act(&action, &observation, None).await?;
    observation = observer.observe().await?;
    ensure!(
        observer
            .page()
            .evaluate("document.getElementById('key-state').value")
            .await?
            == "ArrowDown received",
        "key dispatch failed"
    );

    let action = find_action(&observation, "click", "Open owned popup")?;
    observer.act(&action, &observation, None).await?;
    observation = observer.observe().await?;
    ensure!(
        observation["title"] == "Owned popup reached",
        "popup was not adopted"
    );

    observer
        .page()
        .evaluate("localStorage.setItem('starkbot-profile-spike','persisted'); true")
        .await?;
    println!(
        "browser parity ({transport_name}): PASS · 2-site OOPIF · {} actions observed on popup · {} protocol calls · {} ms",
        observation["actions"].as_array().map_or(0, Vec::len),
        browser.calls(),
        started.elapsed().as_millis()
    );
    browser.close().await;

    let mut headed = LaunchOptions::new(profile.path());
    headed.headless = false;
    headed.transport = transport;
    let relaunched = Instant::now();
    let browser = Browser::launch(&headed).await?;
    let page = browser.new_page(&format!("{base_url}/popup.html")).await?;
    ensure!(
        page.evaluate("localStorage.getItem('starkbot-profile-spike')")
            .await?
            == "persisted",
        "managed Chrome profile did not persist storage across launches"
    );
    println!(
        "headed persistent profile: PASS · relaunch {} ms",
        relaunched.elapsed().as_millis()
    );
    browser.close().await;
    fixture_server.abort();
    frame_server.abort();
    Ok(())
}
