//! Who answers the cards a run publishes, and what the suite scores about them
//! (16 §5.3, §6.2).
//!
//! A navigator run no longer dies when it meets something it may not do alone:
//! it publishes `AppEvent::ConfirmRequest` or `AppEvent::AskRequest` and waits
//! for a person. An eval has no person, so the harness stands in for one — and
//! that is the *measurement*, not a workaround. A case can then assert the two
//! things that matter and could not be asserted before:
//!
//! - the card was published at all, with the cause the rules layer says it has
//!   (`label:pay`, `upload:127.0.0.1`, an ask carrying the sign-in sentence);
//! - approving it **completed the work**, which the page's own record proves.
//!
//! # Why a policy and not an auto-yes
//!
//! Approving everything would make the confirm gate untestable: the run that
//! must stop at a payment and the run that must sail through a save would be
//! indistinguishable. So every case declares what the person watching would
//! do, the harness does exactly that, and the record of what it answered is
//! attached to the run as the `cards` tool call — the same shape as `fixture`
//! and `probe`, scored with `ExpectToolArg` or a custom assertion.
//!
//! # Why the default is a refusal
//!
//! A case that trips a card it never declared is a case whose *rules
//! behaviour* is not what its author thought. Denying it keeps the run alive
//! (a denial becomes a steer, `agent/tools.rs`) and leaves the card in the
//! record, so the report says which rule fired. Approving by default would
//! quietly launder exactly the escalation the review set exists to catch.

use std::sync::{Arc, Mutex};

use neo_agent::Runtime;
use neo_core::events::{AskView, ConfirmView, GateOutcome, ResolutionVia};
use serde_json::{Value, json};
use spice_framework::agent::AgentConfig;

/// What the person watching would do, per case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cards {
    pub confirm: Confirm,
    pub ask: Option<Ask>,
}

/// The answer to a confirm card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Do the thing that was asked about.
    Approve,
    /// Don't. The run carries on and has to find another way.
    Deny,
}

/// The answer to a question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// "I'm ready" — the hand-over answer, for a run that asked the person to
    /// do something only they can do.
    Ready,
    /// A field value the goal did not describe.
    Value(String),
    /// Do the human part first, then say ready: open the fixture's session on
    /// `url`'s origin, which is what a real sign-in would have done, and only
    /// then resume. Without this a resumed run re-observes the same login
    /// wall, and the case proves nothing about resuming.
    SignIn { url: String },
}

impl Default for Cards {
    fn default() -> Self {
        Self {
            confirm: Confirm::Deny,
            ask: None,
        }
    }
}

impl Cards {
    /// Approve whatever this case trips, and answer no questions.
    #[must_use]
    pub const fn approving() -> Self {
        Self {
            confirm: Confirm::Approve,
            ask: None,
        }
    }

    /// Stand in for a person who signs in when asked.
    #[must_use]
    pub fn signing_in(url: impl Into<String>) -> Self {
        Self {
            confirm: Confirm::Deny,
            ask: Some(Ask::SignIn { url: url.into() }),
        }
    }

    #[must_use]
    pub fn from_config(config: &AgentConfig) -> Option<Self> {
        let cards = config.data.get("cards")?;
        let confirm = match cards.get("confirm").and_then(Value::as_str) {
            Some("approve") => Confirm::Approve,
            _ => Confirm::Deny,
        };
        let ask =
            cards
                .get("ask")
                .and_then(|ask| match ask.get("kind").and_then(Value::as_str)? {
                    "ready" => Some(Ask::Ready),
                    "value" => Some(Ask::Value(
                        ask.get("text").and_then(Value::as_str)?.to_owned(),
                    )),
                    "sign_in" => Some(Ask::SignIn {
                        url: ask.get("url").and_then(Value::as_str)?.to_owned(),
                    }),
                    _ => None,
                });
        Some(Self { confirm, ask })
    }

    #[must_use]
    pub fn config(&self) -> Value {
        let confirm = match self.confirm {
            Confirm::Approve => "approve",
            Confirm::Deny => "deny",
        };
        let ask = match &self.ask {
            None => Value::Null,
            Some(Ask::Ready) => json!({ "kind": "ready" }),
            Some(Ask::Value(text)) => json!({ "kind": "value", "text": text }),
            Some(Ask::SignIn { url }) => json!({ "kind": "sign_in", "url": url }),
        };
        json!({ "cards": { "confirm": confirm, "ask": ask } })
    }
}

/// Every card a run published, and what the harness answered.
#[derive(Clone, Debug, Default)]
pub struct Published {
    confirms: Vec<Value>,
    asks: Vec<Value>,
}

impl Published {
    /// The `cards` tool call's arguments. Every key here is a contract with
    /// the assertions in [`crate::cases`].
    #[must_use]
    pub fn describe(&self) -> Value {
        json!({
            "confirms": self.confirms,
            "asks": self.asks,
            "count": self.confirms.len() + self.asks.len(),
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.confirms.is_empty() && self.asks.is_empty()
    }
}

/// The harness in the person's chair: records the card, answers it, and keeps
/// the record for the case to score.
///
/// Shared because the event subscriber and the turn that attaches the record
/// are two different places; a `Mutex` rather than a channel because the whole
/// value is read once, after the turn, and a lock is never held across an
/// await.
#[derive(Clone)]
pub struct Stand {
    runtime: Arc<Runtime>,
    policy: Cards,
    published: Arc<Mutex<Published>>,
}

impl Stand {
    #[must_use]
    pub fn new(runtime: Arc<Runtime>, policy: Cards) -> Self {
        Self {
            runtime,
            policy,
            published: Arc::new(Mutex::new(Published::default())),
        }
    }

    /// What was published and answered, for the run's record.
    #[must_use]
    pub fn published(&self) -> Published {
        self.published
            .lock()
            .map(|published| published.clone())
            .unwrap_or_default()
    }

    /// Answer one confirm card the way this case's person would.
    pub fn confirm(&self, confirm: &ConfirmView) {
        let (outcome, answered) = match self.policy.confirm {
            Confirm::Approve => (GateOutcome::Confirmed, "confirmed"),
            Confirm::Deny => (GateOutcome::Denied, "denied"),
        };
        // A card the run has already given up on (a timeout, a cancelled
        // suite) answers `NoSuchCard`; that is a fact about the run, and it
        // belongs in the record rather than in an error.
        let delivered = self
            .runtime
            .resolve_confirm(confirm.id, outcome, ResolutionVia::Card)
            .is_ok();
        self.record(
            true,
            json!({
                "cause": confirm.cause,
                "sentence": confirm.action_sentence,
                "context": confirm.context,
                "can_remember": confirm.can_remember,
                "answered": answered,
                "delivered": delivered,
            }),
        );
    }

    /// Answer one question, doing the human part first when the policy says
    /// the person would have.
    pub async fn ask(&self, ask: &AskView) {
        let answer = match &self.policy.ask {
            None => None,
            Some(Ask::Ready) => Some(ready_of(ask)),
            Some(Ask::Value(text)) => Some(text.clone()),
            Some(Ask::SignIn { url }) => {
                // The part only a person could do, done before the answer:
                // the run re-observes on resume, so the page has to have
                // changed by the time it does.
                match crate::pages::open_session(self.runtime.data_dir(), url).await {
                    Ok(()) => Some(ready_of(ask)),
                    // Nothing was signed in, so nothing is claimed; the run
                    // keeps waiting and the case fails with the reason in the
                    // record rather than passing on a lie.
                    Err(_) => None,
                }
            }
        };
        let delivered = answer.as_ref().is_some_and(|answer| {
            self.runtime
                .answer_ask(ask.id, answer.clone(), ResolutionVia::Card)
                .is_ok()
        });
        self.record(
            false,
            json!({
                "question": ask.question,
                "options": ask.options,
                "answered": answer,
                "delivered": delivered,
            }),
        );
    }

    fn record(&self, is_confirm: bool, entry: Value) {
        if let Ok(mut published) = self.published.lock() {
            if is_confirm {
                published.confirms.push(entry);
            } else {
                published.asks.push(entry);
            }
        }
    }
}

/// The one thing there is to say to a hand-over card: whatever option it
/// offered, or the plain sentence when it offered none.
fn ready_of(ask: &AskView) -> String {
    ask.options
        .first()
        .cloned()
        .unwrap_or_else(|| "I'm ready".to_owned())
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use neo_core::Envelope;
    use neo_core::events::AppEvent;
    use tokio::sync::broadcast::Receiver;

    use super::*;

    fn runtime() -> (Arc<Runtime>, Receiver<Envelope>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let runtime = Arc::new(Runtime::open(dir.path()).expect("the store opens"));
        let events = runtime.subscribe();
        (runtime, events, dir)
    }

    /// Drain events the way [`crate::NeoAgent`]'s collector does, until the
    /// card arrives, and answer it.
    async fn answer(stand: &Stand, events: &mut Receiver<Envelope>) {
        loop {
            let envelope = events.recv().await.expect("the channel stays open");
            match &envelope.event {
                AppEvent::ConfirmRequest { confirm } => {
                    stand.confirm(confirm);
                    return;
                }
                AppEvent::AskRequest { ask } => {
                    stand.ask(ask).await;
                    return;
                }
                _ => {}
            }
        }
    }

    /// The property three review-set cases rest on: a run parked on a card
    /// gets the answer this case declared, and the record says which rule
    /// fired and what was said to it. Without the answer the run would sit
    /// there until the card timed out, which is a case that measures the
    /// timeout instead of the gate.
    #[tokio::test]
    async fn an_approval_reaches_the_parked_run_and_is_recorded() {
        let (runtime, mut events, _dir) = runtime();
        let stand = Stand::new(Arc::clone(&runtime), Cards::approving());

        let parked = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime
                    .confirm(
                        neo_core::TaskId::new(),
                        "label:pay".to_owned(),
                        "`Pay 12.00 EUR` says “pay”.".to_owned(),
                        Some("Invoice INV-2291 — http://127.0.0.1:8787/nav-pay.html".to_owned()),
                    )
                    .await
            })
        };
        answer(&stand, &mut events).await;

        assert_eq!(
            parked
                .await
                .expect("the parked run did not panic")
                .expect("the confirm resolves"),
            GateOutcome::Confirmed
        );
        let described = stand.published().describe();
        assert_eq!(described["confirms"][0]["cause"], json!("label:pay"));
        assert_eq!(described["confirms"][0]["answered"], json!("confirmed"));
        assert_eq!(described["confirms"][0]["delivered"], json!(true));
        assert_eq!(described["count"], json!(1));
    }

    /// The default, and the reason it is the default: an unexpected card is
    /// refused, the run stays alive to find another way, and the report still
    /// names the rule that fired.
    #[tokio::test]
    async fn an_undeclared_card_is_refused_rather_than_ignored() {
        let (runtime, mut events, _dir) = runtime();
        let stand = Stand::new(Arc::clone(&runtime), Cards::default());

        let parked = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime
                    .confirm(
                        neo_core::TaskId::new(),
                        "safety:spends".to_owned(),
                        "`Buy now` looks like it spends money.".to_owned(),
                        None,
                    )
                    .await
            })
        };
        answer(&stand, &mut events).await;

        assert_eq!(
            parked
                .await
                .expect("the parked run did not panic")
                .expect("the confirm resolves"),
            GateOutcome::Denied
        );
        assert_eq!(
            stand.published().describe()["confirms"][0]["answered"],
            json!("denied")
        );
    }

    /// A question the goal could not answer: the value the case declared is
    /// what the run types, and the record keeps it so a case can assert the
    /// question was the one it expected.
    #[tokio::test]
    async fn a_question_is_answered_with_the_value_the_case_declared() {
        let (runtime, mut events, _dir) = runtime();
        let stand = Stand::new(
            Arc::clone(&runtime),
            Cards {
                confirm: Confirm::Deny,
                ask: Some(Ask::Value("INV-4417".to_owned())),
            },
        );

        let parked = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime
                    .ask_user(
                        neo_core::TaskId::new(),
                        "Tell me what goes in “Invoice number” and I'll carry on.".to_owned(),
                        Vec::new(),
                    )
                    .await
            })
        };
        answer(&stand, &mut events).await;

        assert_eq!(
            parked
                .await
                .expect("the parked run did not panic")
                .expect("the question resolves"),
            Some("INV-4417".to_owned())
        );
        let described = stand.published().describe();
        assert!(
            described["asks"][0]["question"]
                .as_str()
                .is_some_and(|question| question.contains("Invoice number")),
            "the record must carry the question: {described}"
        );
        assert_eq!(described["asks"][0]["answered"], json!("INV-4417"));
    }

    /// A case that declared no answer to a question leaves it unanswered, on
    /// purpose: the run keeps waiting and the case fails with the card in its
    /// record, rather than passing because the harness invented a value.
    #[tokio::test]
    async fn a_question_no_case_declared_is_left_unanswered() {
        let (runtime, mut events, _dir) = runtime();
        let stand = Stand::new(Arc::clone(&runtime), Cards::default());
        let runtime_for_ask = Arc::clone(&runtime);
        let parked = tokio::spawn(async move {
            runtime_for_ask
                .ask_user(neo_core::TaskId::new(), "Which one?".to_owned(), Vec::new())
                .await
        });
        answer(&stand, &mut events).await;

        let described = stand.published().describe();
        assert_eq!(described["asks"][0]["answered"], Value::Null);
        assert_eq!(described["asks"][0]["delivered"], json!(false));
        assert!(!parked.is_finished(), "the run must still be waiting");
        parked.abort();
    }

    /// A case declares its card policy in config and the adapter reads it
    /// back: the round trip is the contract between `cases.rs` and the run.
    #[test]
    fn a_policy_round_trips_through_the_config() {
        for policy in [
            Cards::default(),
            Cards::approving(),
            Cards::signing_in("http://127.0.0.1:8787/nav-state.html"),
            Cards {
                confirm: Confirm::Approve,
                ask: Some(Ask::Value("INV-4417".to_owned())),
            },
            Cards {
                confirm: Confirm::Deny,
                ask: Some(Ask::Ready),
            },
        ] {
            let config = AgentConfig {
                data: policy.config(),
            };
            assert_eq!(Cards::from_config(&config), Some(policy));
        }
    }

    /// A case that declared no policy must not accidentally approve: the
    /// default refuses, which keeps the run alive without spending anything.
    #[test]
    fn a_case_without_a_policy_refuses_rather_than_approves() {
        assert_eq!(Cards::default().confirm, Confirm::Deny);
        assert_eq!(
            Cards::from_config(&AgentConfig {
                data: json!({ "probe": { "kind": "surface", "app": "Numbers" } }),
            }),
            None,
        );
        // An unknown answer is a refusal too, never an approval.
        assert_eq!(
            Cards::from_config(&AgentConfig {
                data: json!({ "cards": { "confirm": "maybe" } }),
            })
            .map(|cards| cards.confirm),
            Some(Confirm::Deny),
        );
    }

    /// A hand-over card offers exactly one thing to say, and the answer has to
    /// be that thing — `agent/tools.rs` maps any answer with options to
    /// `Approval::Ready`, but a free-text ask maps to a typed value, so the
    /// distinction is what keeps a value out of a hand-over.
    #[test]
    fn a_hand_over_is_answered_with_the_option_it_offered() {
        let ask = |options: Vec<String>| AskView {
            id: neo_core::AskId::new(),
            task_id: neo_core::TaskId::new(),
            question: "Sign in and I'll pick up where I stopped.".to_owned(),
            options,
            voice_window_ends: None,
        };
        assert_eq!(ready_of(&ask(vec!["I'm ready".to_owned()])), "I'm ready");
        assert_eq!(ready_of(&ask(vec![])), "I'm ready");
    }

    /// The record is what a case scores, so an empty run says so and a
    /// published card carries the rule that fired.
    #[test]
    fn the_record_names_the_rule_that_fired() {
        let mut published = Published::default();
        assert!(published.is_empty());
        assert_eq!(published.describe()["count"], json!(0));

        published.confirms.push(json!({
            "cause": "label:pay",
            "answered": "confirmed",
        }));
        assert!(!published.is_empty());
        let described = published.describe();
        assert_eq!(described["confirms"][0]["cause"], json!("label:pay"));
        assert_eq!(described["count"], json!(1));
    }
}
