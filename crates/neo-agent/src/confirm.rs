//! The confirm broker: what makes an escalation a conversation instead of an
//! ending (16 §5.3).
//!
//! A navigator run that meets something it may not do alone — a step the
//! safety heads scored risky, a button whose label says "Pay", a login wall,
//! a field the goal does not describe — pauses. It publishes a card, then
//! *waits here* while the surface stays exactly as it was, and carries on
//! with whatever the person answered.
//!
//! Three properties are load-bearing:
//!
//! - **The run is still alive while it waits.** The tool call has not
//!   returned, the browser tab is still open, and `jev-nav`'s resume token is
//!   still in hand, so an approval executes the action the person actually
//!   looked at rather than a fresh guess at it.
//! - **Every wait ends.** A card nobody answers times out, and a timeout is
//!   a refusal, not a silent execution.
//! - **An answer arrives once.** Resolving takes the waiter out of the table,
//!   so a double-click on a confirm card cannot approve two things.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use neo_core::events::{AppEvent, AskView, ConfirmView, GateOutcome, ResolutionVia};
use neo_core::{AskId, ConfirmId, TaskId};
use tokio::sync::oneshot;

use crate::runtime::{Runtime, RuntimeError, now_ms};

/// How long a card waits for an answer.
///
/// Long enough to walk away from the desk mid-task and come back, short
/// enough that a forgotten run does not hold a browser tab and a Jev budget
/// open for the rest of the day. A card that expires is refused.
pub const CARD_TIMEOUT: Duration = Duration::from_secs(600);

/// A pending card and the run waiting on it.
enum Waiting {
    Confirm(oneshot::Sender<GateOutcome>),
    Ask(oneshot::Sender<Option<String>>),
}

/// The table of cards currently on screen.
#[derive(Default)]
pub struct Broker {
    waiting: Mutex<HashMap<String, Waiting>>,
}

impl Broker {
    fn park(&self, key: String, waiting: Waiting) {
        if let Ok(mut table) = self.waiting.lock() {
            table.insert(key, waiting);
        }
    }

    fn take(&self, key: &str) -> Option<Waiting> {
        self.waiting.lock().ok()?.remove(key)
    }
}

impl Runtime {
    /// Pause a run on a confirm card and answer with whatever the person
    /// decided.
    ///
    /// `cause` is the machine-ish tag a trace and a test can match on
    /// (`safety:spends`, `label:pay`, `upload:forms.test`); `sentence` is the
    /// one line the card shows.
    pub async fn confirm(
        &self,
        task_id: TaskId,
        cause: String,
        sentence: String,
        context: Option<String>,
    ) -> Result<GateOutcome, RuntimeError> {
        let id = ConfirmId::new();
        let (answer, wait) = oneshot::channel();
        self.broker().park(id.to_string(), Waiting::Confirm(answer));
        self.publish(AppEvent::ConfirmRequest {
            confirm: ConfirmView {
                id,
                task_id,
                cause,
                action_sentence: sentence,
                context,
                estimated_cost: None,
                // Remembered allows need a policy table behind them and a way
                // to see and revoke what was remembered; until then every
                // approval is for the action it was asked about (16 §5.3).
                can_remember: false,
                expires_at: now_ms()? + CARD_TIMEOUT.as_millis() as i64,
            },
        });
        // A timeout refuses. The alternative — treating silence as approval —
        // is the one reading of an unanswered question that can spend money.
        let outcome = match tokio::time::timeout(CARD_TIMEOUT, wait).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => GateOutcome::TimedOut,
        };
        self.broker().take(&id.to_string());
        self.publish(AppEvent::ConfirmResolved {
            confirm_id: id,
            outcome,
            via: ResolutionVia::Card,
        });
        Ok(outcome)
    }

    /// Pause a run on a question. `None` means nobody answered in time.
    ///
    /// `options` is empty for a free-text answer (a field value) and carries
    /// the one thing there is to say for a hand-over ("I'm ready"), which is
    /// what lets a front end render the two differently without knowing why
    /// the run stopped.
    pub async fn ask_user(
        &self,
        task_id: TaskId,
        question: String,
        options: Vec<String>,
    ) -> Result<Option<String>, RuntimeError> {
        let id = AskId::new();
        let (answer, wait) = oneshot::channel();
        self.broker().park(id.to_string(), Waiting::Ask(answer));
        self.publish(AppEvent::AskRequest {
            ask: AskView {
                id,
                task_id,
                question,
                options,
                voice_window_ends: None,
            },
        });
        let answer = match tokio::time::timeout(CARD_TIMEOUT, wait).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) | Err(_) => None,
        };
        self.broker().take(&id.to_string());
        self.publish(AppEvent::AskResolved {
            ask_id: id,
            answer: answer.clone().unwrap_or_default(),
            via: ResolutionVia::Card,
        });
        Ok(answer)
    }

    /// A front end's answer to a confirm card.
    ///
    /// `Err(RuntimeError::NoSuchCard)` when the card is already gone — the
    /// run finished, timed out, or another surface answered first. Both front
    /// ends can be showing the same card; the first answer wins and the
    /// second is told so rather than silently doing nothing.
    pub fn resolve_confirm(
        &self,
        id: ConfirmId,
        outcome: GateOutcome,
        via: ResolutionVia,
    ) -> Result<(), RuntimeError> {
        match self.broker().take(&id.to_string()) {
            Some(Waiting::Confirm(answer)) => {
                let _ = answer.send(outcome);
                // The waiting run publishes `ConfirmResolved` with the
                // outcome it actually used, so nothing is announced twice.
                let _ = via;
                Ok(())
            }
            Some(other) => {
                self.broker().park(id.to_string(), other);
                Err(RuntimeError::NoSuchCard(id.to_string()))
            }
            None => Err(RuntimeError::NoSuchCard(id.to_string())),
        }
    }

    /// A front end's answer to a question.
    pub fn answer_ask(
        &self,
        id: AskId,
        answer: String,
        via: ResolutionVia,
    ) -> Result<(), RuntimeError> {
        match self.broker().take(&id.to_string()) {
            Some(Waiting::Ask(sender)) => {
                let _ = sender.send(Some(answer));
                let _ = via;
                Ok(())
            }
            Some(other) => {
                self.broker().park(id.to_string(), other);
                Err(RuntimeError::NoSuchCard(id.to_string()))
            }
            None => Err(RuntimeError::NoSuchCard(id.to_string())),
        }
    }

    /// Say that nobody is going to answer this question.
    ///
    /// A surface that cannot ask — a piped `neo nav`, a front end shutting
    /// down — must be able to say so, or the run sits on a card for the full
    /// timeout waiting for a person who is not there.
    pub fn cancel_ask(&self, id: AskId) -> Result<(), RuntimeError> {
        match self.broker().take(&id.to_string()) {
            Some(Waiting::Ask(sender)) => {
                let _ = sender.send(None);
                Ok(())
            }
            Some(other) => {
                self.broker().park(id.to_string(), other);
                Err(RuntimeError::NoSuchCard(id.to_string()))
            }
            None => Err(RuntimeError::NoSuchCard(id.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use std::sync::Arc;

    use neo_core::Envelope;
    use tokio::sync::broadcast::Receiver;

    use super::*;

    fn runtime() -> (Arc<Runtime>, Receiver<Envelope>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let runtime = Arc::new(Runtime::open(dir.path()).expect("the store opens"));
        let events = runtime.subscribe();
        (runtime, events, dir)
    }

    /// The card id a front end needs is the one the event carried: there is no
    /// other way to answer, so a request that does not publish one is a card
    /// nobody can resolve.
    async fn next_confirm(events: &mut Receiver<Envelope>) -> ConfirmId {
        loop {
            let envelope = events.recv().await.expect("the channel stays open");
            if let AppEvent::ConfirmRequest { confirm } = envelope.event {
                return confirm.id;
            }
        }
    }

    #[tokio::test]
    async fn an_approval_reaches_the_waiting_run() {
        let (runtime, mut events, _dir) = runtime();
        let waiter = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime
                    .confirm(
                        TaskId::new(),
                        "safety:spends".into(),
                        "`Pay $42.00` looks like it spends money.".into(),
                        Some("Checkout — https://shop.test/pay".into()),
                    )
                    .await
            })
        };

        let id = next_confirm(&mut events).await;
        runtime
            .resolve_confirm(id, GateOutcome::Confirmed, ResolutionVia::Card)
            .expect("the card is on screen");

        let outcome = waiter
            .await
            .expect("the waiting task did not panic")
            .expect("the confirm resolves");
        assert_eq!(outcome, GateOutcome::Confirmed);
    }

    /// The property that keeps a double-click from approving two things.
    #[tokio::test]
    async fn a_card_can_only_be_answered_once() {
        let (runtime, mut events, _dir) = runtime();
        let waiter = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime
                    .confirm(
                        TaskId::new(),
                        "label:delete".into(),
                        "`Delete`.".into(),
                        None,
                    )
                    .await
            })
        };

        let id = next_confirm(&mut events).await;
        runtime
            .resolve_confirm(id, GateOutcome::Denied, ResolutionVia::Card)
            .expect("the first answer lands");
        let second = runtime.resolve_confirm(id, GateOutcome::Confirmed, ResolutionVia::Card);

        assert!(
            matches!(second, Err(RuntimeError::NoSuchCard(_))),
            "a second answer must be refused, not applied: {second:?}"
        );
        assert_eq!(
            waiter
                .await
                .expect("the waiting task did not panic")
                .expect("the confirm resolves"),
            GateOutcome::Denied,
            "the run used the first answer"
        );
    }

    /// An answer for a card that no run is waiting on — a stale front end, a
    /// finished run — is refused rather than swallowed.
    #[tokio::test]
    async fn answering_a_card_nobody_is_waiting_on_is_an_error() {
        let (runtime, _events, _dir) = runtime();

        let answered = runtime.resolve_confirm(
            ConfirmId::new(),
            GateOutcome::Confirmed,
            ResolutionVia::Card,
        );

        assert!(matches!(answered, Err(RuntimeError::NoSuchCard(_))));
    }

    /// A free-text answer comes back verbatim: this is the value that gets
    /// typed into the field, so anything else is a bug with a keystroke
    /// attached.
    #[tokio::test]
    async fn a_free_text_answer_comes_back_verbatim() {
        let (runtime, mut events, _dir) = runtime();
        let waiter = {
            let runtime = Arc::clone(&runtime);
            tokio::spawn(async move {
                runtime
                    .ask_user(
                        TaskId::new(),
                        "Tell me what goes in “Invoice number”.".into(),
                        Vec::new(),
                    )
                    .await
            })
        };

        let id = loop {
            let envelope = events.recv().await.expect("the channel stays open");
            if let AppEvent::AskRequest { ask } = envelope.event {
                assert!(ask.options.is_empty(), "a free-text ask offers no options");
                break ask.id;
            }
        };
        runtime
            .answer_ask(id, "INV-4417".into(), ResolutionVia::Card)
            .expect("the question is on screen");

        assert_eq!(
            waiter
                .await
                .expect("the waiting task did not panic")
                .expect("the question resolves"),
            Some("INV-4417".to_owned())
        );
    }
}
