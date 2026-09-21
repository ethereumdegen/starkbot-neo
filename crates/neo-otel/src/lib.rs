//! OpenTelemetry instrumentation for Starkbot Neo.
//!
//! Neo is an instrumented application, not a client of any particular trace
//! tool. It builds OpenTelemetry spans for its own work — an agent turn, a
//! step, an inference round trip, a navigator run and each of its decisions
//! — and exports them over OTLP/HTTP to whatever `OTEL_EXPORTER_OTLP_ENDPOINT`
//! names: an OpenTelemetry Collector, Jaeger, Tempo, Honeycomb, Raindrop, or
//! Starkbot Trace. Nothing here knows which, and nothing here depends on any
//! of them.
//!
//! With no endpoint configured there is no exporter, no background task and
//! no work at any call site. That is the default, and it is the reason this
//! can be called from the hot path without a second thought.
//!
//! Three standard environment variables configure it, because a user who has
//! configured any other OpenTelemetry producer has already configured this
//! one:
//!
//! `OTEL_EXPORTER_OTLP_ENDPOINT` — where to post, e.g. `http://localhost:4318`.
//! `OTEL_EXPORTER_OTLP_HEADERS` — `k=v,k2=v2`, for a hosted endpoint's key.
//! `OTEL_SERVICE_NAME` — overrides `starkbot-neo` in the resource.
//!
//! # Privacy
//!
//! Enforced here and at the call sites, not by the receiver. No credential
//! ever becomes an attribute. Text typed into a field is counted
//! (`starkbot.typed_chars`) and thrown away — a field being typed into may
//! hold a password or a one-time code. A prompt is counted
//! (`starkbot.prompt_chars`), never copied. Goals, thoughts, observations
//! and answers *are* recorded: they are what a trace is for.

mod export;
mod span;
mod tracer;

pub use span::{SpanBuilder, SpanKind};
pub use tracer::{
    Attached, annotate, attach, attached, event, fail, flush, in_span, init, new_span_id,
    new_trace_id, record, record_attached, shutdown, surface,
};
