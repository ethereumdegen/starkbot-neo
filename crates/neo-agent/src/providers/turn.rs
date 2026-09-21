//! What one finished subscription-backed turn carries, for both OAuth
//! inference providers.

use serde_json::Value;

/// One finished turn on a plan-backed provider.
///
/// `usage` is the vendor's own usage object, verbatim — `Value::Null` when the
/// vendor reported none. Plan work is [`neo_core`]'s `Usd::Unpriced` (05 §7):
/// these counts are never turned into Starkbot spend, they are kept so the
/// allowance a plan reports can be shown as the vendor stated it.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// Every assistant text block of the turn, joined with newlines.
    pub text: String,
    /// The model the vendor says answered, which may differ from the one asked
    /// for (an alias resolving to a dated snapshot).
    pub model: String,
    /// The vendor's usage object, unmodified.
    pub usage: Value,
    /// Wall time of the one HTTP round trip that produced this turn.
    pub duration_ms: u64,
}
