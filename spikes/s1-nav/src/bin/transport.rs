use std::time::Instant;

use neo_cdp::{Browser, DebugTransport, LaunchOptions};
use serde_json::json;

const SAMPLES: usize = 200;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    for transport in [DebugTransport::Pipe, DebugTransport::WebSocket] {
        let profile = tempfile::tempdir()?;
        let mut options = LaunchOptions::new(profile.path());
        options.transport = transport;
        let launched = Instant::now();
        let browser = Browser::launch(&options).await?;
        browser.call("Browser.getVersion", json!({})).await?;
        let ready_ms = launched.elapsed().as_millis();
        for _ in 0..5 {
            browser.call("Browser.getVersion", json!({})).await?;
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = Instant::now();
            browser.call("Browser.getVersion", json!({})).await?;
            samples.push(started.elapsed().as_micros());
        }
        samples.sort_unstable();
        let name = match transport {
            DebugTransport::Pipe => "pipe",
            DebugTransport::WebSocket => "websocket",
        };
        println!(
            "{name}: ready={ready_ms} ms calls={SAMPLES} median={} µs p95={} µs",
            samples[SAMPLES / 2],
            samples[SAMPLES * 95 / 100]
        );
        browser.close().await;
    }
    Ok(())
}
