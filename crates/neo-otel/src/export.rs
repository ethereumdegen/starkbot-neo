//! The batching exporter: spans in from anywhere, one HTTP POST out.
//!
//! This is the half of an OpenTelemetry SDK that actually has to be right,
//! and it is written here rather than taken from `opentelemetry-otlp` for the
//! reason given in [`crate::span`] — the transport is `reqwest` posting JSON,
//! which Neo already links, against `tonic`, `prost` and a protobuf toolchain
//! it does not.
//!
//! The behaviour is a production SDK's, because the failure modes are the
//! ones that hurt: a queue that is unbounded turns a collector outage into an
//! out-of-memory kill, so it is bounded and drops; a producer that awaits its
//! exporter turns collector latency into agent latency, so nothing on the hot
//! path ever awaits this; a retry loop without a ceiling turns a broken
//! endpoint into an infinite loop, so it stops at four attempts; and a retry
//! on a 400 is a retry that will never succeed, so only 429 and 5xx are
//! retried, honouring `Retry-After` when the server sends one.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::span;

/// How many spans may be waiting to be sent. Two thousand spans is a few
/// megabytes at Neo's span sizes and several minutes of a busy agent: past
/// that, the collector is gone and keeping more only costs the user memory.
const QUEUE: usize = 2_048;

/// A batch is sent when it reaches this many spans, or when the timer fires,
/// whichever comes first.
const BATCH: usize = 200;
const FLUSH_EVERY: Duration = Duration::from_secs(2);

/// Four attempts, starting at a quarter second and doubling. A longer ladder
/// would outlive the `neo ask` that produced the spans.
const ATTEMPTS: u32 = 4;
const BACKOFF: Duration = Duration::from_millis(250);

/// How long a shutdown waits for the last batch. A command must exit even
/// when the endpoint is a black hole.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

pub(crate) enum Message {
    Span(Value),
    Flush,
    Shutdown(oneshot::Sender<()>),
}

/// What a caller holds: a sender, and a count of what did not fit.
pub(crate) struct Exporter {
    sender: mpsc::Sender<Message>,
    dropped: Arc<AtomicU64>,
}

impl Exporter {
    /// Start the background task, or report why there will not be one.
    ///
    /// A tokio runtime has to be in context: the exporter is a task, and a
    /// tracer initialised outside a runtime would have nowhere to run it.
    /// That is a programming error in a front end, not a user's problem, so
    /// it says so on stderr and leaves tracing off rather than panicking in
    /// a program that was only trying to answer a question.
    pub(crate) fn spawn(endpoint: &str, headers: HeaderMap, resource: Vec<Value>) -> Option<Self> {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            eprintln!(
                "neo-otel: no tokio runtime when tracing was initialised, so no spans will be exported"
            );
            return None;
        };
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                eprintln!("neo-otel: no HTTP client, so no spans will be exported: {error}");
                return None;
            }
        };
        let (sender, receiver) = mpsc::channel(QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let task = Task {
            receiver,
            client,
            url: traces_url(endpoint),
            headers,
            resource,
            dropped: Arc::clone(&dropped),
        };
        handle.spawn(task.run());
        Some(Self { sender, dropped })
    }

    /// Hand one finished span over. Never blocks and never fails: a full
    /// queue means the collector is not keeping up, and the agent's work
    /// matters more than its telemetry.
    pub(crate) fn queue(&self, span: Value) {
        if self.sender.try_send(Message::Span(span)).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Ask for a send now, without waiting for it. The sync exit paths use
    /// this; anything that can await uses [`Exporter::shutdown`], which is
    /// the only one of the two that guarantees delivery.
    pub(crate) fn flush(&self) {
        let _ = self.sender.try_send(Message::Flush);
    }

    /// Send what is left and stop. Bounded, because a command that hangs on
    /// exit is worse than a command that loses a span.
    pub(crate) async fn shutdown(&self) {
        let (ack, acked) = oneshot::channel();
        if self.sender.send(Message::Shutdown(ack)).await.is_err() {
            return;
        }
        let _ = tokio::time::timeout(SHUTDOWN_WAIT, acked).await;
    }

    #[cfg(test)]
    pub(crate) fn channel(capacity: usize) -> (Self, mpsc::Receiver<Message>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            Self {
                sender,
                dropped: Arc::new(AtomicU64::new(0)),
            },
            receiver,
        )
    }

    #[cfg(test)]
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The background half: everything that can be slow happens here.
struct Task {
    receiver: mpsc::Receiver<Message>,
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    resource: Vec<Value>,
    dropped: Arc<AtomicU64>,
}

impl Task {
    async fn run(mut self) {
        let mut batch: Vec<Value> = Vec::with_capacity(BATCH);
        let mut ticker = tokio::time::interval(FLUSH_EVERY);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                message = self.receiver.recv() => match message {
                    Some(Message::Span(span)) => {
                        batch.push(span);
                        if batch.len() >= BATCH {
                            self.send(&mut batch).await;
                        }
                    }
                    Some(Message::Flush) => self.send(&mut batch).await,
                    Some(Message::Shutdown(ack)) => {
                        self.send(&mut batch).await;
                        self.report_drops();
                        let _ = ack.send(());
                        return;
                    }
                    // Every sender is gone: the process is on its way out and
                    // the last batch is still worth a try.
                    None => {
                        self.send(&mut batch).await;
                        self.report_drops();
                        return;
                    }
                },
                _ = ticker.tick() => self.send(&mut batch).await,
            }
        }
    }

    async fn send(&self, batch: &mut Vec<Value>) {
        if batch.is_empty() {
            return;
        }
        let spans = std::mem::take(batch);
        let count = spans.len();
        // Serialised once and reused across attempts: a retry re-sends the
        // same bytes, and rebuilding the document each time would be pure
        // work in the path that is already having a bad day.
        let body = span::document(&self.resource, spans).to_string();
        for attempt in 0..ATTEMPTS {
            let request = self
                .client
                .post(&self.url)
                .header(CONTENT_TYPE, "application/json")
                .headers(self.headers.clone())
                .body(body.clone());
            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return;
                    }
                    if status.as_u16() != 429 && !status.is_server_error() {
                        eprintln!(
                            "neo-otel: {} refused {count} span(s) with {status}; dropping them",
                            self.url
                        );
                        return;
                    }
                    let wait = retry_after(&response).unwrap_or_else(|| backoff(attempt));
                    tokio::time::sleep(wait).await;
                }
                // A refused connection is the endpoint being down, not the
                // request being wrong, so it retries on the same ladder.
                Err(error) => {
                    if attempt + 1 == ATTEMPTS {
                        eprintln!(
                            "neo-otel: {count} span(s) not delivered to {}: {error}",
                            self.url
                        );
                        return;
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                }
            }
        }
        eprintln!(
            "neo-otel: {} would not accept {count} span(s) after {ATTEMPTS} attempts; dropping them",
            self.url
        );
    }

    /// Said once, on the way out: a dropped span that nobody is told about is
    /// a report with a hole in it that reads like a quiet agent.
    fn report_drops(&self) {
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            eprintln!("neo-otel: dropped {dropped} span(s) because the export queue was full");
        }
    }
}

/// Exponential with ±20% jitter, so a fleet of Neos that all saw the same
/// 503 do not all come back in the same millisecond.
fn backoff(attempt: u32) -> Duration {
    let base = u64::try_from(BACKOFF.as_millis())
        .unwrap_or(u64::MAX)
        .saturating_mul(1 << attempt.min(8));
    let spread = base / 5;
    // The clock is the jitter source rather than a random number generator:
    // a dependency on `rand` to decide how long to sleep is not worth its
    // weight, and the low bits of the nanosecond clock differ between
    // processes, which is the only property this needs.
    let nanos = u64::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos()),
    );
    let jitter = nanos % (spread * 2 + 1);
    Duration::from_millis(base.saturating_add(jitter).saturating_sub(spread))
}

/// `Retry-After` in seconds, which is the form every collector sends it in.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(reqwest::header::RETRY_AFTER)?;
    let seconds: u64 = value.to_str().ok()?.trim().parse().ok()?;
    // A server asking for an hour is a server asking us to give up; the
    // ceiling keeps one bad header from stalling the exporter.
    Some(Duration::from_secs(seconds.min(30)))
}

/// The endpoint with the signal's path on it.
///
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is the base — `http://localhost:4318` — and
/// the traces signal lives under `/v1/traces`. An endpoint that already names
/// the path is left alone, because writing the full URL is what everybody
/// does the first time and posting to `/v1/traces/v1/traces` is a 404 nobody
/// enjoys diagnosing.
pub(crate) fn traces_url(endpoint: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    if base.ends_with("/v1/traces") {
        return base.to_owned();
    }
    format!("{base}/v1/traces")
}

/// `k=v,k2=v2`, the form `OTEL_EXPORTER_OTLP_HEADERS` is documented in.
/// A malformed pair is skipped with a line rather than taking the whole
/// exporter down: one bad header should not cost a user their traces.
pub(crate) fn parse_headers(raw: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for pair in raw
        .split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
    {
        let Some((key, value)) = pair.split_once('=') else {
            eprintln!("neo-otel: ignoring OTLP header `{pair}`, which has no `=`");
            continue;
        };
        match (
            HeaderName::from_bytes(key.trim().as_bytes()),
            HeaderValue::from_str(value.trim()),
        ) {
            (Ok(name), Ok(value)) => {
                headers.insert(name, value);
            }
            _ => eprintln!("neo-otel: ignoring OTLP header `{key}`, which HTTP will not carry"),
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_signal_path_is_appended_once() {
        assert_eq!(
            traces_url("http://localhost:4318"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            traces_url("http://localhost:4318/"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            traces_url("http://localhost:4318/v1/traces"),
            "http://localhost:4318/v1/traces"
        );
    }

    #[test]
    fn headers_come_from_the_documented_comma_separated_form() {
        let headers = parse_headers("x-api-key=secret, x-tenant=neo");
        assert_eq!(
            headers.get("x-api-key").map(|value| value.as_bytes()),
            Some(&b"secret"[..])
        );
        assert_eq!(
            headers.get("x-tenant").map(|value| value.as_bytes()),
            Some(&b"neo"[..])
        );
        assert_eq!(parse_headers("nonsense").len(), 0);
    }

    /// The ladder has to grow and it has to stay inside the jitter band, or a
    /// retry storm is one bad multiplication away.
    #[test]
    fn backoff_grows_and_stays_within_its_jitter_band() {
        for attempt in 0..ATTEMPTS {
            let base = BACKOFF.as_millis() << attempt;
            let spread = base * 2 / 5;
            let wait = backoff(attempt).as_millis();
            assert!(
                wait >= base - spread && wait <= base + spread,
                "attempt {attempt} waited {wait} ms, outside {base} ms ±20%"
            );
        }
    }
}
