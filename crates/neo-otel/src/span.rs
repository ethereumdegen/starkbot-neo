//! The OTLP document Neo puts on the wire, and the builder one span is filled in with.
//!
//! This is OTLP/HTTP with JSON encoding — the encoding an OpenTelemetry
//! Collector accepts on port 4318 without any configuration, and the one
//! Raindrop, Honeycomb, Jaeger and Tempo all read. It is written out by hand
//! with `serde_json` rather than through the `opentelemetry` crates on
//! purpose: the protobuf encoding would bring `prost`, `tonic` and a build
//! time protoc into a program whose entire telemetry need is "post a
//! document", and `opentelemetry_sdk` is a second async runtime's worth of
//! machinery beside the one Neo already runs. The schema is stable and
//! documented, so writing it directly costs a few hundred lines once and
//! nothing thereafter.
//!
//! Two details of the encoding are easy to get wrong and both are load
//! bearing. Timestamps are decimal *strings* of nanoseconds, because a JSON
//! number is a double and nanoseconds since 1970 pass 2^53 in 1970 plus a
//! few months. Integer attributes are strings for the same reason: a token
//! count is small today, but a rowid or a byte count is not, and a collector
//! that silently rounded one would be worse than one that refused it.

use std::time::Duration;

use serde_json::{Map, Value, json};

/// How many events one span will carry.
///
/// Events accumulate on the live builder until the span closes, and
/// `Runtime::publish` routes every `AppEvent` onto whatever span is open.
/// `AppEvent::TurnDelta` is published once per streamed token slice, so a
/// turn that answered in two thousand tokens once built a span holding two
/// thousand events, each carrying the whole serialized event — the answer
/// over again, slice by slice. That span went out in one POST that most
/// collectors refuse outright, and the exporter then dropped the batch
/// whole. The cap turns "the whole turn is missing" into "the tail of a very
/// chatty turn is missing", and `starkbot.events_dropped` says so on the
/// span rather than leaving a reader to guess.
///
/// The publish site skips the high-rate variants as well; this is the bound
/// that holds when a new one is added and nobody remembers to.
const MAX_EVENTS: usize = 128;

/// The longest string an attribute value will carry, in bytes.
///
/// Not every value is written by hand: a serialized `AppEvent` arrives as
/// arbitrary JSON and is rendered into one string, and an answer or a page
/// of text is as long as the model felt like being. Four kilobytes is more
/// than a reader uses and small enough that one span stays in the kilobytes
/// whatever it was handed.
const MAX_ATTRIBUTE_BYTES: usize = 4_096;

/// The span kinds Neo produces. A turn, a step and a navigator run are work
/// this process did itself; a model round trip is a call out to somebody
/// else, and a reader wants to see that distinction without parsing names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Internal,
    Client,
}

impl SpanKind {
    const fn code(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::Client => 3,
        }
    }
}

/// One span, being filled in.
///
/// The builder carries no identity and no clock: a span's trace id, span id,
/// parent and timestamps are decided by the tracer when it is opened or
/// recorded, because only the tracer knows what is already open. What the
/// builder holds is everything the *call site* knows — the name, the kind,
/// how long the work took, its attributes, its events and whether it failed.
#[derive(Clone, Debug)]
pub struct SpanBuilder {
    name: String,
    kind: SpanKind,
    elapsed: Duration,
    attributes: Vec<Value>,
    events: Vec<Value>,
    /// How many events [`MAX_EVENTS`] refused, reported on the span itself.
    dropped_events: u64,
    error: Option<String>,
}

impl SpanBuilder {
    /// Work this process did itself.
    #[must_use]
    pub fn internal(name: impl Into<String>) -> Self {
        Self::new(name, SpanKind::Internal)
    }

    /// A call out to another service — an inference round trip.
    #[must_use]
    pub fn client(name: impl Into<String>) -> Self {
        Self::new(name, SpanKind::Client)
    }

    fn new(name: impl Into<String>, kind: SpanKind) -> Self {
        Self {
            name: name.into(),
            kind,
            elapsed: Duration::ZERO,
            attributes: Vec::new(),
            events: Vec::new(),
            dropped_events: 0,
            error: None,
        }
    }

    /// How long the work this span describes took.
    ///
    /// Only meaningful for a span that is recorded after the fact: a span
    /// opened around a future is timed by the tracer, which is more accurate
    /// than anything the call site could pass.
    #[must_use]
    pub const fn elapsed(mut self, elapsed: Duration) -> Self {
        self.elapsed = elapsed;
        self
    }

    #[must_use]
    pub fn text(mut self, key: &str, value: impl Into<String>) -> Self {
        self.push(key, Value::String(value.into()));
        self
    }

    /// An attribute that may not exist. Absent stays absent: a token count
    /// the vendor did not report is not a token count of zero.
    #[must_use]
    pub fn maybe_text(mut self, key: &str, value: Option<impl Into<String>>) -> Self {
        if let Some(value) = value {
            self.push(key, Value::String(value.into()));
        }
        self
    }

    /// An integer attribute, or nothing.
    ///
    /// A value that will not fit in an `i64` is left off the span rather
    /// than clamped. This used to saturate to `i64::MAX`, which is wrong in
    /// a way that reads as right: `TryInto<i64>` fails for an out-of-range
    /// *negative* as readily as a positive, so a number below `i64::MIN`
    /// arrived at the collector as the largest positive integer there is.
    /// An absent attribute is a hole a reader can see; a sign flip is not.
    #[must_use]
    pub fn int<V: TryInto<i64>>(mut self, key: &str, value: V) -> Self {
        if let Ok(value) = value.try_into() {
            self.push(key, Value::from(value));
        }
        self
    }

    #[must_use]
    pub fn maybe_int<V: TryInto<i64>>(self, key: &str, value: Option<V>) -> Self {
        match value {
            Some(value) => self.int(key, value),
            None => self,
        }
    }

    #[must_use]
    pub fn float(mut self, key: &str, value: f64) -> Self {
        self.push(key, json!(value));
        self
    }

    #[must_use]
    pub fn maybe_float(self, key: &str, value: Option<f64>) -> Self {
        match value {
            Some(value) => self.float(key, value),
            None => self,
        }
    }

    #[must_use]
    pub fn flag(mut self, key: &str, value: bool) -> Self {
        self.push(key, Value::Bool(value));
        self
    }

    /// Mark the work as failed. The message is what a reader sees beside the
    /// red span, so it is the error's own words, not a code.
    #[must_use]
    pub fn failed(mut self, message: impl Into<String>) -> Self {
        self.error = Some(message.into());
        self
    }

    /// The same as [`SpanBuilder::failed`] for a result whose error side is
    /// already rendered, so a call site does not need a `match` to say
    /// "whatever went wrong, say so".
    #[must_use]
    pub fn outcome<T>(self, result: Result<T, impl std::fmt::Display>) -> Self {
        match result {
            Ok(_) => self,
            Err(error) => self.failed(error.to_string()),
        }
    }

    /// A null carries nothing a reader can use and OTLP has no type for it —
    /// a float that came out `NaN` is the one way one gets here — so the
    /// attribute is left off rather than written as the string "null".
    pub(crate) fn push(&mut self, key: &str, value: Value) {
        if value.is_null() {
            return;
        }
        self.attributes.push(attribute(key, value));
    }

    /// Note that something happened inside this span, up to [`MAX_EVENTS`]
    /// times. Past the cap the event is counted and discarded, because a
    /// span nobody can receive carries less than a truncated one.
    pub(crate) fn push_event(&mut self, event: Value) {
        if self.events.len() >= MAX_EVENTS {
            self.dropped_events += 1;
            return;
        }
        self.events.push(event);
    }

    pub(crate) fn set_failed(&mut self, message: &str) {
        self.error = Some(message.to_owned());
    }

    pub(crate) fn nanos(&self) -> u128 {
        self.elapsed.as_nanos()
    }

    /// The finished span as OTLP sees it. The tracer supplies identity and
    /// the clock; everything else was decided at the call site.
    pub(crate) fn finish(
        mut self,
        trace_id: &str,
        span_id: &str,
        parent: Option<&str>,
        start_unix_nano: u128,
        end_unix_nano: u128,
    ) -> Value {
        if self.dropped_events > 0 {
            // On the span, not in a log line: the reader who needs to know
            // the event list is incomplete is the one looking at the
            // waterfall, and they are not reading this process's stderr.
            let dropped = self.dropped_events;
            self.push("starkbot.events_dropped", Value::from(dropped));
        }
        let mut span = Map::new();
        span.insert("traceId".to_owned(), Value::String(trace_id.to_owned()));
        span.insert("spanId".to_owned(), Value::String(span_id.to_owned()));
        if let Some(parent) = parent {
            span.insert("parentSpanId".to_owned(), Value::String(parent.to_owned()));
        }
        span.insert("name".to_owned(), Value::String(self.name));
        span.insert("kind".to_owned(), Value::from(self.kind.code()));
        span.insert(
            "startTimeUnixNano".to_owned(),
            Value::String(start_unix_nano.to_string()),
        );
        span.insert(
            "endTimeUnixNano".to_owned(),
            Value::String(end_unix_nano.to_string()),
        );
        span.insert("attributes".to_owned(), Value::Array(self.attributes));
        span.insert("events".to_owned(), Value::Array(self.events));
        span.insert(
            "status".to_owned(),
            match self.error {
                Some(message) => json!({ "code": 2, "message": message }),
                None => json!({ "code": 1 }),
            },
        );
        Value::Object(span)
    }
}

/// One `{"key":…,"value":{…}}` pair.
#[must_use]
pub(crate) fn attribute(key: &str, value: Value) -> Value {
    json!({ "key": key, "value": any_value(value) })
}

/// A JSON value as an OTLP `AnyValue`.
///
/// Integers become strings, for the reason given at the top of this module.
/// A composite value is rendered rather than dropped: an `AppEvent`'s payload
/// is arbitrary JSON, and a reader would rather see `{"run":"…"}` than
/// nothing at all — bounded, because that payload is written by whatever
/// produced the event and not by anybody thinking about span size.
fn any_value(value: Value) -> Value {
    match value {
        Value::String(text) => json!({ "stringValue": clamp(text) }),
        Value::Bool(flag) => json!({ "boolValue": flag }),
        Value::Number(number) => match (number.as_i64(), number.as_f64()) {
            (Some(integer), _) => json!({ "intValue": integer.to_string() }),
            (None, Some(double)) => json!({ "doubleValue": double }),
            (None, None) => json!({ "stringValue": number.to_string() }),
        },
        other => json!({ "stringValue": clamp(other.to_string()) }),
    }
}

/// A string attribute value, no longer than [`MAX_ATTRIBUTE_BYTES`].
///
/// Marked rather than silent: a value that just stops is a value a reader
/// will believe, so the elision says how much of it is missing. Bytes are
/// never fewer than characters, so the common case — every attribute
/// written by hand in this repo — is one length compare and no scan.
fn clamp(text: String) -> String {
    if text.len() <= MAX_ATTRIBUTE_BYTES {
        return text;
    }
    let mut end = MAX_ATTRIBUTE_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let elided = text.len() - end;
    let mut clamped = text;
    clamped.truncate(end);
    clamped.push_str(&format!("… ({elided} more bytes elided)"));
    clamped
}

/// One span event: something that happened at an instant inside a span.
pub(crate) fn event(name: &str, attributes: &[Value], at_unix_nano: u128) -> Value {
    json!({
        "timeUnixNano": at_unix_nano.to_string(),
        "name": name,
        "attributes": attributes,
    })
}

/// The attribute list of a span event, built from raw JSON values.
pub(crate) fn attributes(pairs: Vec<(&'static str, Value)>) -> Vec<Value> {
    pairs
        .into_iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| attribute(key, value))
        .collect()
}

/// The one document a batch of spans is posted as.
///
/// Every span in a batch shares this process's resource and one scope, which
/// is what a single-service producer always sends: the resource says who
/// produced the spans, the scope says which instrumentation did.
pub(crate) fn document(resource: &[Value], spans: Vec<Value>) -> Value {
    json!({
        "resourceSpans": [{
            "resource": { "attributes": resource },
            "scopeSpans": [{
                "scope": { "name": "starkbot-neo", "version": env!("CARGO_PKG_VERSION") },
                "spans": spans,
            }],
        }],
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    /// The shape is the contract with every collector on the other end, so it
    /// is asserted literally: a renamed field or an integer that stopped
    /// being a string is a silent data loss at the receiver, not a test
    /// failure anybody would otherwise see.
    #[test]
    fn a_span_serialises_to_the_documented_otlp_json() {
        let span = SpanBuilder::client("chat claude-sonnet-4-5")
            .text("gen_ai.operation.name", "chat")
            .text("gen_ai.system", "anthropic")
            .int("gen_ai.usage.input_tokens", 1_200_u32)
            .maybe_int("gen_ai.usage.output_tokens", None::<u32>)
            .float("starkbot.confidence", 0.5)
            .flag("starkbot.json_mode", true)
            .failed("the vendor refused")
            .finish(
                "0123456789abcdef0123456789abcdef",
                "0123456789abcdef",
                None,
                7,
                9,
            );

        let resource = vec![attribute(
            "service.name",
            Value::String("starkbot-neo".into()),
        )];
        let document = document(&resource, vec![span]);

        let expected = json!({
            "resourceSpans": [{
                "resource": {
                    "attributes": [
                        { "key": "service.name", "value": { "stringValue": "starkbot-neo" } }
                    ]
                },
                "scopeSpans": [{
                    "scope": { "name": "starkbot-neo", "version": env!("CARGO_PKG_VERSION") },
                    "spans": [{
                        "traceId": "0123456789abcdef0123456789abcdef",
                        "spanId": "0123456789abcdef",
                        "name": "chat claude-sonnet-4-5",
                        "kind": 3,
                        "startTimeUnixNano": "7",
                        "endTimeUnixNano": "9",
                        "attributes": [
                            { "key": "gen_ai.operation.name", "value": { "stringValue": "chat" } },
                            { "key": "gen_ai.system", "value": { "stringValue": "anthropic" } },
                            { "key": "gen_ai.usage.input_tokens", "value": { "intValue": "1200" } },
                            { "key": "starkbot.confidence", "value": { "doubleValue": 0.5 } },
                            { "key": "starkbot.json_mode", "value": { "boolValue": true } }
                        ],
                        "events": [],
                        "status": { "code": 2, "message": "the vendor refused" }
                    }]
                }]
            }]
        });
        assert_eq!(document, expected);
    }

    #[test]
    fn a_parent_and_an_event_are_carried_and_a_healthy_span_is_status_one() {
        let mut span = SpanBuilder::internal("invoke_agent");
        span.push_event(event(
            "app_event.turn_started",
            &attributes(vec![("starkbot.event", json!({ "type": "turn_started" }))]),
            11,
        ));
        let span = span.finish(
            "a".repeat(32).as_str(),
            "b".repeat(16).as_str(),
            Some("c"),
            1,
            2,
        );

        assert_eq!(span["parentSpanId"], json!("c"));
        assert_eq!(span["status"], json!({ "code": 1 }));
        assert_eq!(span["events"][0]["name"], json!("app_event.turn_started"));
        assert_eq!(span["events"][0]["timeUnixNano"], json!("11"));
        assert_eq!(
            span["events"][0]["attributes"][0]["value"],
            json!({ "stringValue": "{\"type\":\"turn_started\"}" })
        );
    }

    /// Every `AppEvent` lands on whatever span is open and `TurnDelta`
    /// arrives once per streamed token slice, so an unbounded event list is
    /// a multi-megabyte span the collector refuses whole. The cap has to
    /// hold, and the span has to say how much it swallowed — a short event
    /// list that claims to be complete is worse than one that does not.
    #[test]
    fn a_span_keeps_only_its_first_events_and_says_how_many_it_dropped() {
        let mut span = SpanBuilder::internal("invoke_agent");
        for index in 0..MAX_EVENTS + 50 {
            span.push_event(event(
                "app_event.turn_delta",
                &attributes(vec![("starkbot.seq", Value::from(index))]),
                u128::try_from(index).unwrap_or(0),
            ));
        }
        let span = span.finish("a".repeat(32).as_str(), "b".repeat(16).as_str(), None, 1, 2);

        let events = span["events"].as_array().map(Vec::len);
        assert_eq!(events, Some(MAX_EVENTS), "the event list grew past its cap");
        // The first events are the ones kept: a turn's `turn_started` is
        // worth more than the two-thousandth slice of its answer.
        assert_eq!(span["events"][0]["timeUnixNano"], json!("0"));
        assert_eq!(
            span["attributes"][0],
            json!({ "key": "starkbot.events_dropped", "value": { "intValue": "50" } })
        );
    }

    /// An attribute value is not always written by hand: a serialized
    /// `AppEvent`, an answer or a page of text arrives here at whatever
    /// length its producer felt like. The bound is on bytes, and it holds
    /// without splitting a character in half.
    #[test]
    fn a_long_attribute_value_is_truncated_on_a_character_boundary_and_says_so() {
        // Two bytes per character, so a byte cap lands mid-character unless
        // it is walked back — and an invalid `String` is not constructible,
        // so getting this wrong is a panic, not a bad span.
        let long = "é".repeat(MAX_ATTRIBUTE_BYTES);
        let rendered = attribute("starkbot.answer", Value::String(long.clone()));
        let value = rendered["value"]["stringValue"]
            .as_str()
            .unwrap_or_default();

        assert!(value.starts_with("éé"), "the head of the value was lost");
        assert!(
            value.len() < long.len(),
            "a {}-byte value was carried whole",
            long.len()
        );
        let tail: String = value
            .chars()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert!(
            value.contains("more bytes elided"),
            "the truncation was silent; the value ends `{tail}`"
        );

        // A value inside the bound is untouched, marker and all.
        let short = attribute("starkbot.answer", Value::String("é".repeat(8)));
        assert_eq!(short["value"]["stringValue"], json!("é".repeat(8)));
    }
}
