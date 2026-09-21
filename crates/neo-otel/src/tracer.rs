//! The in-process tracer: what is open, what it is a child of, and where it goes.
//!
//! # Why a task-local and not a context argument
//!
//! A span has to know its parent. The obvious way to arrange that is a
//! context value threaded through every function that might produce a span —
//! and in Neo that is `Runtime::ask`, `ask_json`, `run_browser`, `run_app`,
//! every `jev-nav` step callback, and `Runtime::publish`, which is called
//! from a dozen places that have no idea a trace exists. Threading a
//! parameter through all of them changes public signatures that have nothing
//! to do with telemetry, forces every caller to have a context to pass even
//! when tracing is off, and still misses the interesting case: the navigator
//! callbacks are closures handed to another crate, which cannot be given a
//! new argument at all.
//!
//! A `tokio::task_local!` holding the open span solves it where the problem
//! actually is. A turn scopes the task-local for the duration of its future;
//! everything awaited inside that future — inference, tool runs, navigator
//! steps — sees it without being told, and everything outside sees nothing
//! and becomes a root span, which is exactly right for a bare `neo nav`.
//! This is the same shape OpenTelemetry's own `Context` takes, minus the
//! SDK. The one rule it imposes: a span produced from a `tokio::spawn`ed
//! task is a root, because a spawned task does not inherit its parent's
//! task-locals. Nothing in Neo produces spans from a detached task today,
//! and a root span is a degradation rather than a failure if something does.
//!
//! # Why nothing costs anything when tracing is off
//!
//! With no `OTEL_EXPORTER_OTLP_ENDPOINT`, [`init`] leaves the tracer
//! disabled and every entry point here returns before it allocates a span
//! id, scopes a task-local or touches a queue. An unobserved Neo pays one
//! atomic load per call site.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::export::{self, Exporter};
use crate::span::{self, SpanBuilder};

/// The process's tracer. One per process, because the resource attributes —
/// which program, which surface, which run — are decided once at startup.
static TRACER: OnceLock<Tracer> = OnceLock::new();

/// The surface name [`init`] was given. Separate from `TRACER` so it is
/// readable when tracing is disabled, which is the default.
static SURFACE: OnceLock<String> = OnceLock::new();

tokio::task_local! {
    /// The innermost span open on this task.
    static CURRENT: Arc<Open>;
}

/// A span that has been opened and not yet closed.
struct Open {
    trace_id: String,
    span_id: String,
    parent: Option<String>,
    started_unix_nano: u128,
    /// `None` once the span has been closed, which happens exactly once. A
    /// late event — something that fires as the future unwinds — is dropped
    /// rather than resurrecting a span that has already been sent.
    builder: Mutex<Option<SpanBuilder>>,
}

/// The process's tracer: the resource lives with the exporter, because the
/// exporter is the only thing that needs it, and a disabled tracer holds
/// nothing at all.
pub(crate) struct Tracer {
    exporter: Option<Exporter>,
}

/// Name the process and read the environment. Idempotent: the first call
/// wins, and a second one — a test, a front end that initialises twice — is
/// a no-op rather than a second exporter.
///
/// `surface` is `neo-cli`, `neo-tui` or `neo-desktop`: which front end this
/// process is, which is the first thing anybody reading a trace wants to
/// know.
pub fn init(surface: &str) {
    // Recorded whether or not an exporter exists: the screen lease reads it
    // to name a refusal, and that has to work with tracing switched off.
    let _ = SURFACE.set(surface.to_owned());
    let _ = TRACER.get_or_init(|| Tracer::from_environment(surface));
}

/// Which front end this process is, as [`init`] named it.
///
/// The screen lease puts this in its refusals, and "neo-desktop is running
/// `drive TextEdit`" is the difference between a user finding the window and
/// giving up. Falls back to `"neo"` before `init` runs.
#[must_use]
pub fn surface() -> &'static str {
    SURFACE.get().map_or("neo", String::as_str)
}

/// A new trace id: 32 lowercase hex characters, as OTLP requires.
///
/// Random, not time-ordered: a v7 uuid spends its first twelve hex characters
/// on a millisecond timestamp, so every trace from one run looked alike and a
/// reader asking for a trace by a short prefix got somebody else's spans
/// folded into the waterfall.
#[must_use]
pub fn new_trace_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// A new span id: 16 lowercase hex characters.
#[must_use]
pub fn new_span_id() -> String {
    // The low half of a v7 uuid is its random half; the high half is a
    // timestamp, and using it would make every span id in a millisecond
    // share a prefix.
    let uuid = uuid::Uuid::now_v7();
    let mut id = String::with_capacity(16);
    for byte in &uuid.as_bytes()[8..] {
        // Hex by hand rather than another dependency for sixteen characters.
        id.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        id.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    id
}

/// Run `future` inside `span`: the span is opened now, closed when the
/// future resolves, and everything traced inside it becomes its child.
pub async fn in_span<T>(span: SpanBuilder, future: impl Future<Output = T>) -> T {
    match enabled() {
        Some(tracer) => tracer.in_span(span, future).await,
        None => future.await,
    }
}

/// The span open on this task, so work that leaves the task can be put back
/// inside it.
///
/// The rule in this module's header — a span produced from a `tokio::spawn`ed
/// task is a root, because a spawned task does not inherit its parent's
/// task-locals — stopped being free the moment a turn's graph started running
/// on a task `metalcraft::Executor::stream` spawns. Everything the graph does
/// (every tool run, every navigator step) is then detached, and a turn's
/// trace showed the turn with none of its own work under it. There is no way
/// to make the spawn inherit the task-local from outside metalcraft, so the
/// span is carried by hand instead: capture it before the work leaves, and
/// [`attach`] it again inside.
///
/// Captured outside any span — which is the normal case, and every case when
/// tracing is off — it attaches nothing, and the work roots exactly as it
/// does today.
#[derive(Clone, Default)]
pub struct Attached(Option<Arc<Open>>);

/// Capture the span this task is inside, to [`attach`] on another task.
#[must_use]
pub fn attached() -> Attached {
    Attached(CURRENT.try_with(Arc::clone).ok())
}

/// Run `future` inside the span [`attached`] captured, as though it had never
/// left that task.
pub async fn attach<T>(attached: Attached, future: impl Future<Output = T>) -> T {
    match attached.0 {
        Some(open) => CURRENT.scope(open, future).await,
        None => future.await,
    }
}

/// Record work that has already happened and was timed by its caller — one
/// navigator step, one inference round trip. Parented to whatever span this
/// task is inside, or a root of its own when it is inside none.
pub fn record(span: SpanBuilder) {
    if let Some(tracer) = enabled() {
        tracer.record(span, None);
    }
}

/// [`record`], parented to a span [`attached`] captured elsewhere.
///
/// The synchronous half of [`attach`], for a caller that cannot await: a
/// per-call hook handed to another crate is a plain `Fn`, and it fires on
/// the task that crate spawned, where the task-local is not.
pub fn record_attached(attached: &Attached, span: SpanBuilder) {
    if let Some(tracer) = enabled() {
        tracer.record(span, attached.0.as_deref());
    }
}

/// Note that something happened at an instant, on the innermost open span.
///
/// With no span open — a key changing while the user is typing, a notice
/// from a background poll — the event becomes a zero-length root span, so
/// it is still in the trace rather than silently discarded.
pub fn event(name: &str, attributes: Vec<(&'static str, Value)>) {
    if let Some(tracer) = enabled() {
        tracer.event(name, attributes);
    }
}

/// Add attributes to the span this task is inside.
///
/// A turn's outcome — how many steps it took, whether it answered or asked —
/// is only known when the turn is over, by which time the future that opened
/// the span is the only thing holding it. This is the active-span API an
/// OpenTelemetry SDK exposes for exactly that reason.
pub fn annotate(attributes: Vec<(&'static str, Value)>) {
    if enabled().is_none() {
        return;
    }
    with_open(|builder| {
        for (key, value) in attributes {
            builder.push(key, value);
        }
    });
}

/// Mark the span this task is inside as failed.
pub fn fail(message: &str) {
    if enabled().is_none() {
        return;
    }
    with_open(|builder| builder.set_failed(message));
}

/// Edit the innermost span still open on this task, if there is one. A span
/// that has already been closed and sent is past editing, and saying so
/// here keeps every caller from having to think about it.
fn with_open(edit: impl FnOnce(&mut SpanBuilder)) {
    let _ = CURRENT.try_with(|open| {
        if let Ok(mut builder) = open.builder.lock()
            && let Some(builder) = builder.as_mut()
        {
            edit(builder);
        }
    });
}

/// Ask for everything queued to be sent, without waiting. For an exit path
/// that cannot await; [`shutdown`] is the one that guarantees delivery.
pub fn flush() {
    if let Some(exporter) = enabled().and_then(|tracer| tracer.exporter.as_ref()) {
        exporter.flush();
    }
}

/// Send what is left and stop. Called on the way out of a command, because a
/// `neo ask` finishes in well under the two-second flush interval and would
/// otherwise take its spans to the grave.
pub async fn shutdown() {
    if let Some(exporter) = enabled().and_then(|tracer| tracer.exporter.as_ref()) {
        exporter.shutdown().await;
    }
}

/// The tracer, if there is one and it has somewhere to send spans.
fn enabled() -> Option<&'static Tracer> {
    TRACER.get().filter(|tracer| tracer.exporter.is_some())
}

impl Tracer {
    fn from_environment(surface: &str) -> Self {
        let service = std::env::var("OTEL_SERVICE_NAME")
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "starkbot-neo".to_owned());
        let resource = vec![
            span::attribute("service.name", Value::String(service)),
            span::attribute(
                "service.version",
                Value::String(env!("CARGO_PKG_VERSION").to_owned()),
            ),
            span::attribute("process.pid", Value::from(std::process::id())),
            span::attribute("starkbot.surface", Value::String(surface.to_owned())),
            // One id for this process, so a report can separate two Neos
            // running side by side without guessing from timestamps.
            span::attribute(
                "starkbot.run",
                Value::String(uuid::Uuid::now_v7().to_string()),
            ),
        ];
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .unwrap_or_default()
            .trim()
            .to_owned();
        if endpoint.is_empty() {
            // No endpoint is the normal case, not a misconfiguration: a Neo
            // nobody is watching should cost nothing at all.
            return Self { exporter: None };
        }
        let headers =
            export::parse_headers(&std::env::var("OTEL_EXPORTER_OTLP_HEADERS").unwrap_or_default());
        Self {
            exporter: Exporter::spawn(&endpoint, headers, resource),
        }
    }

    async fn in_span<T>(&self, span: SpanBuilder, future: impl Future<Output = T>) -> T {
        let (trace_id, parent) = context();
        let open = Arc::new(Open {
            trace_id,
            span_id: new_span_id(),
            parent,
            started_unix_nano: now_unix_nano(),
            builder: Mutex::new(Some(span)),
        });
        let result = CURRENT.scope(Arc::clone(&open), future).await;
        self.close(&open);
        result
    }

    fn close(&self, open: &Open) {
        let Some(builder) = open.builder.lock().ok().and_then(|mut held| held.take()) else {
            return;
        };
        self.queue(builder.finish(
            &open.trace_id,
            &open.span_id,
            open.parent.as_deref(),
            open.started_unix_nano,
            now_unix_nano(),
        ));
    }

    /// `under` names the parent explicitly, for a caller that captured the
    /// span before the work left its task; `None` reads this task's own.
    fn record(&self, span: SpanBuilder, under: Option<&Open>) {
        let (trace_id, parent) = match under {
            Some(open) => (open.trace_id.clone(), Some(open.span_id.clone())),
            None => context(),
        };
        let end = now_unix_nano();
        let start = end.saturating_sub(span.nanos());
        self.queue(span.finish(&trace_id, &new_span_id(), parent.as_deref(), start, end));
    }

    fn event(&self, name: &str, attributes: Vec<(&'static str, Value)>) {
        let attributes = span::attributes(attributes);
        let at = now_unix_nano();
        let recorded = CURRENT
            .try_with(|open| match open.builder.lock() {
                Ok(mut builder) => match builder.as_mut() {
                    Some(builder) => {
                        builder.push_event(span::event(name, &attributes, at));
                        true
                    }
                    None => false,
                },
                Err(_) => false,
            })
            .unwrap_or(false);
        if recorded {
            return;
        }
        let mut orphan = SpanBuilder::internal(name);
        orphan.push_event(span::event(name, &attributes, at));
        self.queue(orphan.finish(&new_trace_id(), &new_span_id(), None, at, at));
    }

    fn queue(&self, span: Value) {
        if let Some(exporter) = &self.exporter {
            exporter.queue(span);
        }
    }
}

/// The trace this task belongs to and the span to hang a new one under.
fn context() -> (String, Option<String>) {
    CURRENT
        .try_with(|open| (open.trace_id.clone(), Some(open.span_id.clone())))
        .unwrap_or_else(|_| (new_trace_id(), None))
}

/// Nanoseconds since the Unix epoch, the only clock OTLP speaks. A clock
/// before 1970 is a machine whose spans nobody can order anyway, so it
/// becomes zero rather than a failure.
fn now_unix_nano() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    use std::time::Duration;

    use crate::export::Message;

    fn tracer(capacity: usize) -> (Tracer, tokio::sync::mpsc::Receiver<Message>) {
        let (exporter, receiver) = Exporter::channel(capacity);
        (
            Tracer {
                exporter: Some(exporter),
            },
            receiver,
        )
    }

    fn queued(receiver: &mut tokio::sync::mpsc::Receiver<Message>) -> Vec<Value> {
        let mut spans = Vec::new();
        while let Ok(Message::Span(span)) = receiver.try_recv() {
            spans.push(span);
        }
        spans
    }

    #[test]
    fn ids_are_lowercase_hex_of_the_widths_otlp_requires() {
        for _ in 0..8 {
            let trace = new_trace_id();
            let span = new_span_id();
            assert_eq!(trace.len(), 32, "trace id `{trace}`");
            assert_eq!(span.len(), 16, "span id `{span}`");
            assert!(
                trace
                    .chars()
                    .chain(span.chars())
                    .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "`{trace}` / `{span}` is not lowercase hex"
            );
        }
        assert_ne!(new_span_id(), new_span_id());
    }

    /// Parenting is the whole point of the task-local: a step, an inference
    /// and a navigator run all have to land under the turn that caused them
    /// without being handed anything.
    #[tokio::test]
    async fn a_span_opened_inside_another_is_its_child_in_the_same_trace() {
        let (tracer, mut receiver) = tracer(16);
        tracer
            .in_span(SpanBuilder::internal("invoke_agent"), async {
                tracer
                    .in_span(SpanBuilder::internal("execute_tool"), async {
                        tracer.record(
                            SpanBuilder::client("chat sol").elapsed(Duration::from_millis(5)),
                            None,
                        );
                    })
                    .await;
            })
            .await;

        let spans = queued(&mut receiver);
        assert_eq!(spans.len(), 3, "one turn, one step, one inference");
        let inference = &spans[0];
        let step = &spans[1];
        let turn = &spans[2];
        assert_eq!(turn["name"], "invoke_agent");
        assert_eq!(turn["parentSpanId"], Value::Null);
        assert_eq!(step["parentSpanId"], turn["spanId"]);
        assert_eq!(inference["parentSpanId"], step["spanId"]);
        assert_eq!(step["traceId"], turn["traceId"]);
        assert_eq!(inference["traceId"], turn["traceId"]);

        // A recorded span's start is its measured duration before its end.
        let start: u128 = inference["startTimeUnixNano"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .expect("a decimal nanosecond string");
        let end: u128 = inference["endTimeUnixNano"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .expect("a decimal nanosecond string");
        assert_eq!(end - start, Duration::from_millis(5).as_nanos());
    }

    /// The task-local cannot cross a `tokio::spawn`, and the chat graph runs
    /// on a task metalcraft spawns. Both halves of the carry have to work or
    /// a turn's tool runs and model calls each start a trace of their own:
    /// the async [`attach`], which the tool's work runs inside, and the
    /// synchronous [`Tracer::record`] under an explicit parent, which is
    /// what a per-call hook has to use.
    #[tokio::test]
    async fn work_on_a_spawned_task_stays_under_the_span_it_was_carried_from() {
        let (tracer, mut receiver) = tracer(16);
        let tracer = Arc::new(tracer);
        let outer = Arc::clone(&tracer);
        outer
            .in_span(SpanBuilder::internal("invoke_agent"), async {
                let carried = attached();
                let detached = Arc::clone(&tracer);
                tokio::spawn(async move {
                    // Nothing is inherited here: without the carry both of
                    // these would be roots.
                    detached.record(SpanBuilder::client("chat sol"), carried.0.as_deref());
                    attach(carried, async {
                        detached.record(SpanBuilder::internal("navigate app"), None);
                    })
                    .await;
                })
                .await
                .expect("the spawned task runs to completion");
            })
            .await;

        let spans = queued(&mut receiver);
        assert_eq!(spans.len(), 3, "one turn, one inference, one navigation");
        let inference = &spans[0];
        let navigation = &spans[1];
        let turn = &spans[2];
        assert_eq!(turn["name"], "invoke_agent");
        assert_eq!(inference["parentSpanId"], turn["spanId"]);
        assert_eq!(navigation["parentSpanId"], turn["spanId"]);
        assert_eq!(inference["traceId"], turn["traceId"]);
        assert_eq!(navigation["traceId"], turn["traceId"]);
    }

    /// An `AppEvent` inside a turn belongs to the turn; one with nothing open
    /// still has to reach the collector.
    #[tokio::test]
    async fn an_event_lands_on_the_open_span_or_becomes_a_root_of_its_own() {
        let (tracer, mut receiver) = tracer(16);
        tracer
            .in_span(SpanBuilder::internal("invoke_agent"), async {
                tracer.event(
                    "app_event.turn_step",
                    vec![("starkbot.step", Value::from(1))],
                );
            })
            .await;
        let spans = queued(&mut receiver);
        assert_eq!(spans.len(), 1, "the event did not open a span of its own");
        assert_eq!(spans[0]["events"][0]["name"], "app_event.turn_step");
        assert_eq!(
            spans[0]["events"][0]["attributes"][0]["value"],
            serde_json::json!({ "intValue": "1" })
        );

        tracer.event("app_event.key_status", Vec::new());
        let spans = queued(&mut receiver);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["name"], "app_event.key_status");
        assert_eq!(spans[0]["startTimeUnixNano"], spans[0]["endTimeUnixNano"]);
    }

    /// With no endpoint there is no exporter, and the cheap path has to stay
    /// cheap: no task-local scope, no span ids, no queue.
    #[tokio::test]
    async fn a_disabled_tracer_records_nothing_and_opens_no_scope() {
        assert!(enabled().is_none(), "no test initialises an endpoint");
        let inside = in_span(SpanBuilder::internal("invoke_agent"), async {
            CURRENT.try_with(|_| ()).is_ok()
        })
        .await;
        assert!(!inside, "a disabled tracer must not scope the task-local");
        record(SpanBuilder::internal("execute_tool"));
        event("app_event.notice", Vec::new());
        annotate(vec![("starkbot.answer", Value::from("hello"))]);
        fail("nothing is listening");
        flush();
    }

    /// A collector that stops reading must cost memory that is bounded, and
    /// the drop has to be counted so it can be reported at exit.
    #[test]
    fn the_queue_drops_rather_than_growing_past_its_bound() {
        let (exporter, mut receiver) = Exporter::channel(2);
        for index in 0..5 {
            exporter.queue(Value::from(index));
        }
        assert_eq!(exporter.dropped(), 3);
        let mut held = 0;
        while receiver.try_recv().is_ok() {
            held += 1;
        }
        assert_eq!(held, 2, "the queue grew past its bound");
    }

    /// The outcome attributes of a turn are only known once the turn is
    /// over, so they are set on the span while it is still open.
    #[tokio::test]
    async fn a_span_can_be_annotated_from_inside_the_work_it_covers() {
        let (tracer, mut receiver) = tracer(4);
        tracer
            .in_span(SpanBuilder::internal("invoke_agent"), async {
                with_open(|builder| builder.push("starkbot.steps", Value::from(2)));
                with_open(|builder| builder.set_failed("the vendor refused"));
            })
            .await;
        let spans = queued(&mut receiver);
        assert_eq!(spans[0]["attributes"][0]["key"], "starkbot.steps");
        assert_eq!(
            spans[0]["attributes"][0]["value"],
            serde_json::json!({ "intValue": "2" })
        );
        assert_eq!(
            spans[0]["status"],
            serde_json::json!({ "code": 2, "message": "the vendor refused" })
        );
    }
}
