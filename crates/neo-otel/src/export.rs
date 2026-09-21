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
    /// it is logged and tracing stays off, rather than panicking in a
    /// program that was only trying to answer a question.
    ///
    /// Every diagnostic in this module goes through `tracing` and not
    /// stderr. The TUI calls `init("neo-tui")` from inside ratatui's
    /// alternate screen, and a `println` there paints over the frame the
    /// user is reading — which is exactly what a telemetry failure must not
    /// cost them.
    pub(crate) fn spawn(endpoint: &str, headers: HeaderMap, resource: Vec<Value>) -> Option<Self> {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                "no tokio runtime when tracing was initialised, so no spans will be exported"
            );
            return None;
        };
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                tracing::warn!(%error, "no HTTP client, so no spans will be exported");
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

    /// Send what is left and stop. Bounded, because a command that hangs on
    /// exit is worse than a command that loses a span.
    ///
    /// The only exit path. There used to be a non-awaiting `flush` beside
    /// this for "a caller that cannot await", and in the whole tree there
    /// was no such caller: it was a second delivery contract with nothing
    /// behind it.
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
                        tracing::warn!(
                            url = %self.url,
                            %status,
                            count,
                            "the collector refused the batch; dropping it"
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
                        tracing::warn!(
                            url = %self.url,
                            count,
                            %error,
                            "the batch was not delivered"
                        );
                        return;
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                }
            }
        }
        tracing::warn!(
            url = %self.url,
            count,
            attempts = ATTEMPTS,
            "the collector would not accept the batch; dropping it"
        );
    }

    /// Said once, on the way out: a dropped span that nobody is told about is
    /// a report with a hole in it that reads like a quiet agent.
    fn report_drops(&self) {
        let dropped = self.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            tracing::warn!(
                dropped,
                "spans were dropped because the export queue was full"
            );
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
///
/// The line never carries the pair. `OTEL_EXPORTER_OTLP_HEADERS` is where a
/// hosted collector's API key lives, and a token with no `=` in it cannot
/// be split into a name and a value — so there is nothing in it that is
/// known not to be the credential, and its position is all a user needs to
/// find it. A key that HTTP will not carry is named, because a header name
/// is not a secret.
pub(crate) fn parse_headers(raw: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (index, pair) in raw
        .split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .enumerate()
    {
        let Some((key, value)) = pair.split_once('=') else {
            tracing::warn!(
                position = index + 1,
                "ignoring an OTLP header with no `=` in it"
            );
            continue;
        };
        match (
            HeaderName::from_bytes(key.trim().as_bytes()),
            HeaderValue::from_str(value.trim()),
        ) {
            (Ok(name), Ok(value)) => {
                headers.insert(name, value);
            }
            _ => tracing::warn!(
                key = key.trim(),
                "ignoring an OTLP header HTTP will not carry"
            ),
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

    /// One span, as the tracer would have finished it.
    fn one_span(name: &str) -> Value {
        span::SpanBuilder::internal(name).finish(&"a".repeat(32), &"b".repeat(16), None, 1, 2)
    }

    fn resource() -> Vec<Value> {
        vec![span::attribute(
            "service.name",
            Value::String("neo-otel-test".into()),
        )]
    }

    /// Queue one span at `endpoint` and push it out. Every transport test
    /// wants exactly this: nothing the exporter does is observable from
    /// inside the process, so the collector's view is the whole assertion.
    async fn export_one(endpoint: &str, headers: HeaderMap, name: &str) {
        let exporter = Exporter::spawn(endpoint, headers, resource())
            .expect("an exporter, since this test runs inside a tokio runtime");
        exporter.queue(one_span(name));
        exporter.shutdown().await;
    }

    /// A responder that answers `statuses` in order and 200 thereafter, so
    /// a retry ladder can be driven without depending on how `wiremock`
    /// orders two mocks that both match.
    fn statuses(statuses: &'static [u16]) -> impl Fn(&Request) -> ResponseTemplate {
        let seen = Arc::new(AtomicU64::new(0));
        move |_: &Request| {
            let index = usize::try_from(seen.fetch_add(1, Ordering::Relaxed)).unwrap_or(usize::MAX);
            let status = statuses.get(index).copied().unwrap_or(200);
            let template = ResponseTemplate::new(status);
            if status == 429 {
                // A second's wait is far outside the 250 ms ±20% the
                // backoff ladder would have chosen, which is what makes the
                // honouring observable.
                return template.insert_header("retry-after", "1");
            }
            template
        }
    }

    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is a base and the signal path is ours
    /// to append — except when the user pasted the whole URL, which is what
    /// everybody does first. A wrong path is a 404 per batch and a trace
    /// tool that stays empty, and nothing inside the process can tell.
    #[tokio::test]
    async fn a_batch_is_posted_as_json_to_the_signal_path_under_either_spelling() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/traces"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let base = server.uri();
        export_one(&base, HeaderMap::new(), "from the base").await;
        export_one(
            &format!("{base}/v1/traces"),
            HeaderMap::new(),
            "from the full url",
        )
        .await;

        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(requests.len(), 2, "one POST per exporter");
        for request in &requests {
            assert_eq!(request.url.path(), "/v1/traces", "the signal path is wrong");
            assert_eq!(
                request
                    .headers
                    .get("content-type")
                    .and_then(|value| value.to_str().ok()),
                Some("application/json")
            );
            let document: Value =
                serde_json::from_slice(&request.body).expect("an OTLP JSON document");
            let resource = &document["resourceSpans"][0];
            assert_eq!(resource["resource"]["attributes"][0]["key"], "service.name");
            assert!(
                resource["scopeSpans"][0]["spans"][0]["name"].is_string(),
                "the batch carried no span: {document}"
            );
        }
    }

    /// A hosted collector authenticates with `OTEL_EXPORTER_OTLP_HEADERS`.
    /// A header that is parsed and then not attached is a 401 per batch,
    /// and the exporter's own diagnostic for a 401 is a `tracing` line
    /// nobody has a subscriber for.
    #[tokio::test]
    async fn the_configured_headers_travel_on_the_post() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("x-api-key", "secret"))
            .and(header("x-tenant", "neo"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        export_one(
            &server.uri(),
            parse_headers("x-api-key=secret, x-tenant=neo"),
            "authenticated",
        )
        .await;

        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(
            requests.len(),
            1,
            "the matcher on both headers did not see the POST"
        );
    }

    /// 429 is the collector asking for a moment, and the span it refused is
    /// still good. Not retrying it is silent data loss on the one failure
    /// mode a busy agent actually produces.
    #[tokio::test]
    async fn a_rate_limited_batch_is_retried_and_lands() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(statuses(&[429]))
            .mount(&server)
            .await;

        export_one(&server.uri(), HeaderMap::new(), "retried").await;

        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(requests.len(), 2, "the 429 was not retried exactly once");
    }

    /// A 400 will be a 400 on the fourth attempt too: retrying it is four
    /// times the load for the same answer, and on a hosted endpoint four
    /// times the bill.
    #[tokio::test]
    async fn a_refused_batch_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        export_one(&server.uri(), HeaderMap::new(), "refused").await;

        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(requests.len(), 1, "a 400 was retried");
    }

    /// `Retry-After` is the collector telling us when it will be ready. The
    /// backoff ladder would have come back in 250 ms ±20%, so a gap over a
    /// second is the header being read and nothing else.
    #[tokio::test]
    async fn a_retry_after_is_waited_out_rather_than_the_backoff_ladder() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(statuses(&[429]))
            .mount(&server)
            .await;

        let started = std::time::Instant::now();
        export_one(&server.uri(), HeaderMap::new(), "held off").await;
        let elapsed = started.elapsed();

        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            2
        );
        assert!(
            elapsed >= Duration::from_millis(900),
            "the retry came back after {elapsed:?}, which is the backoff ladder, not `Retry-After: 1`"
        );
    }

    /// A collector that asks for an hour is a collector asking us to give
    /// up, and the exporter outlives at most one `neo ask`. Without the
    /// ceiling one header stalls every later batch behind it.
    #[tokio::test]
    async fn a_retry_after_beyond_the_ceiling_is_capped() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "3600")
                    .insert_header("x-case", "capped"),
            )
            .mount(&server)
            .await;
        let capped = reqwest::get(server.uri()).await.expect("a 429 response");
        assert_eq!(retry_after(&capped), Some(Duration::from_secs(30)));

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
            .mount(&server)
            .await;
        let honoured = reqwest::get(server.uri()).await.expect("a 429 response");
        assert_eq!(retry_after(&honoured), Some(Duration::from_secs(2)));

        // `Retry-After` may also be an HTTP date, which no collector sends
        // and this does not parse: the ladder is the fallback, not zero.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT"),
            )
            .mount(&server)
            .await;
        let unparsed = reqwest::get(server.uri()).await.expect("a 429 response");
        assert_eq!(retry_after(&unparsed), None);
    }

    /// A `neo ask` is over in well under [`FLUSH_EVERY`], so without a flush
    /// on the way out its spans die with the process — which is the whole
    /// reason `shutdown` exists. The proof is the batch on the wire before
    /// the timer could have fired, not an ack from a queue.
    #[tokio::test]
    async fn shutdown_puts_the_last_partial_batch_on_the_wire_before_the_timer_would() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let started = std::time::Instant::now();
        export_one(&server.uri(), HeaderMap::new(), "last words").await;
        let elapsed = started.elapsed();

        let requests = server.received_requests().await.unwrap_or_default();
        assert_eq!(requests.len(), 1, "the last batch never left");
        assert!(
            elapsed < FLUSH_EVERY,
            "the batch took {elapsed:?}, so it waited for the timer rather than the shutdown"
        );
        let document: Value = serde_json::from_slice(&requests[0].body).expect("an OTLP document");
        assert_eq!(
            document["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["name"],
            "last words"
        );
    }
}
