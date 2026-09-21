//! The deterministic layer between a Jev decision and the surface (10 §7).
//!
//! Jev's safety heads are a classifier's opinion. This is not: it is a fixed
//! set of rules that run first, cannot be talked out of a refusal by page
//! text, and do not depend on a model answering anything. Three jobs:
//!
//! 1. Read the page's own signals — a visible password field, a challenge
//!    widget — and hand the surface back to the user instead of guessing.
//! 2. Refuse outright what the run may never do: a denied origin, a
//!    non-`http(s)` destination.
//! 3. Require a human confirm for a label that says it sends, spends or
//!    destroys something, whatever the heads said about it.
//!
//! Every outcome here is recoverable by the person watching: that is the
//! point of the layer. Nothing in it ends a run that a human could finish.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::policy::{Action, Decision};
use crate::rules::{ON_TASK, SAFETY};

/// Words in a control's label that make an action a confirm, regardless of
/// what the safety heads believed about it.
///
/// Deliberately about the *label*, not the page: a button that says "Pay" is
/// one a human approves even on a page Jev scored as harmless, and a
/// classifier that is wrong about this is the failure mode with a cost
/// attached. False positives cost one keystroke.
pub const CONFIRM_WORDS: &[&str] = &[
    "pay",
    "buy",
    "purchase",
    "order",
    "checkout",
    "subscribe",
    "upgrade",
    "donate",
    "transfer",
    "withdraw",
    "send",
    "post",
    "publish",
    "share",
    "invite",
    "submit",
    "apply",
    "delete",
    "remove",
    "discard",
    "erase",
    "cancel account",
    "deactivate",
    "unsubscribe",
    "deploy",
    "confirm and",
];

/// The default for [`crate::RunConfig::on_task_floor`], and how many answers
/// below that floor end the run.
///
/// One low answer is normal: an intermediate page — a consent banner, a
/// redirect, a loading screen — is not "relevant to the goal" in any obvious
/// way. Two in a row means the run has wandered, or page text has talked the
/// classifier into somebody else's task, which is what this tripwire is for
/// (10 §6). The floor itself is a setting rather than this constant, because
/// a run that asks for no drift head at all (`0.0`) must not be struck for
/// the answer it never asked for.
pub const ON_TASK_FLOOR: f64 = 0.5;
pub const MAX_OFF_TASK_STRIKES: u32 = 2;

/// What the gate decided about one step.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Nothing in the way; execute it.
    Allow,
    /// A human approves this one action, then the run continues.
    Confirm(ConfirmReason),
    /// Only the person at the keyboard can do the next thing.
    Hand(NeedsUser),
    /// The run may not do this at all.
    Refuse(BlockReason),
}

/// Why an action waits for a person.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmReason {
    /// A safety head crossed the confirm threshold.
    SafetyHead { head: String, probability: f64 },
    /// The control's own label says what it does.
    Label { word: &'static str },
    /// The first upload to an origin in this run (10 §11.3).
    FirstUpload { origin: String },
}

impl ConfirmReason {
    /// One sentence a card can show, in the product's voice: what is about to
    /// happen and why it stopped.
    pub fn sentence(&self, label: &str) -> String {
        match self {
            Self::SafetyHead { head, .. } => match head.as_str() {
                "spends" => format!("`{label}` looks like it spends money."),
                "destructive" => format!("`{label}` looks like it deletes something."),
                _ => format!("`{label}` looks like it sends something to other people."),
            },
            Self::Label { word } => format!("`{label}` says “{word}”."),
            Self::FirstUpload { origin } => format!("First upload to {origin}."),
        }
    }
}

/// What the person has to do before the run can carry on.
#[derive(Debug, Clone, PartialEq)]
pub enum NeedsUser {
    /// A visible password field: the run never types a credential.
    SignIn,
    /// A challenge widget. Not solved, not clicked at — handed over.
    Captcha,
    /// The text helper could not tell what belongs in a field.
    Value { field: String },
}

impl NeedsUser {
    pub fn sentence(&self) -> String {
        match self {
            Self::SignIn => "Sign in and I'll pick up where I stopped.".into(),
            Self::Captcha => "Clear the challenge and I'll carry on.".into(),
            Self::Value { field } => {
                format!("Tell me what goes in “{field}” and I'll carry on.")
            }
        }
    }
}

/// Why a run ended without finishing, when no person can unblock it.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockReason {
    /// Jev reported that no offered operation makes progress.
    NoOperation,
    /// Three actions in a row left the surface unchanged.
    NoProgress,
    /// The `on_task` head fell through the floor twice.
    OffTask,
    /// The run may not touch this host.
    DeniedOrigin { host: String },
    /// A destination that is not a web page.
    NotWeb { scheme: String },
    /// The decision named nothing executable.
    NoAction,
    /// The surface would not hold still, and what the last guard saw move:
    /// "nothing could be executed" alone names no cause, and the cause — an
    /// app that never came forward, a label that rewrites itself, a window
    /// that keeps changing identity — is the whole diagnosis.
    Unstable { reason: String },
    /// A question went out and nothing came back in time.
    Unanswered,
    /// A budget ran out.
    Budget { what: &'static str },
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOperation => f.write_str("nothing on this page makes progress"),
            Self::NoProgress => f.write_str("three actions in a row changed nothing"),
            Self::OffTask => f.write_str("the pages stopped being about the goal"),
            Self::DeniedOrigin { host } => write!(f, "{host} is on the denied list"),
            Self::NotWeb { scheme } => write!(f, "`{scheme}:` is not a web page"),
            Self::NoAction => f.write_str("the decision named no executable action"),
            Self::Unstable { reason } => write!(
                f,
                "the surface changed under every decision; nothing could be executed: {reason}"
            ),
            Self::Unanswered => f.write_str("nobody answered, so I stopped where I was"),
            Self::Budget { what } => write!(f, "the {what} budget ran out"),
        }
    }
}

/// The rules, and the per-run state they need (uploads already approved,
/// off-task strikes).
pub struct Gate {
    /// The probability at which a safety head becomes a confirm.
    pub confirm_at: f64,
    /// Whether the heads were asked at all. A run without them still gets
    /// every deterministic rule: the label list and the page signals do not
    /// depend on a model.
    pub safety_heads: bool,
    /// Hosts this run may never act on.
    pub denied_origins: Vec<String>,
    approved_uploads: Vec<String>,
    off_task_strikes: u32,
}

impl Gate {
    pub fn new(confirm_at: f64, safety_heads: bool, denied_origins: Vec<String>) -> Self {
        Self {
            confirm_at,
            safety_heads,
            denied_origins,
            approved_uploads: Vec::new(),
            off_task_strikes: 0,
        }
    }

    /// What the page itself demands, before any decision is considered.
    ///
    /// Runs on every observation: a login wall or a challenge appearing
    /// mid-run is the common case, not the opening one.
    pub fn page(&self, observation: &Value) -> Option<NeedsUser> {
        let count = |name: &str| observation["signals"][name].as_u64().unwrap_or(0);
        if count("captcha") > 0 {
            return Some(NeedsUser::Captcha);
        }
        if count("password_fields") > 0 {
            return Some(NeedsUser::SignIn);
        }
        None
    }

    /// What one decision is allowed to do.
    pub fn action(&mut self, decision: &Decision, action: &Action, page_url: &str) -> Verdict {
        if let Some(verdict) = self.destination(action) {
            return verdict;
        }
        // A wait is not an action anybody needs to approve.
        let kind = action
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind == "wait" || kind == "scroll" {
            return Verdict::Allow;
        }
        let label = action
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(word) = confirm_word(label) {
            return Verdict::Confirm(ConfirmReason::Label { word });
        }
        if kind == "upload" {
            let origin = origin_of(action, page_url);
            if !self.approved_uploads.contains(&origin) {
                return Verdict::Confirm(ConfirmReason::FirstUpload { origin });
            }
        }
        if self.safety_heads {
            for &(head, _) in SAFETY {
                // Fail closed (A-Q7): `policy::resolve` refuses a response
                // that omitted a head, so an absent one here means nobody
                // wired it — and the unknown scores as risky.
                let probability = decision.safety.get(head).copied().unwrap_or(1.0);
                if probability >= self.confirm_at {
                    return Verdict::Confirm(ConfirmReason::SafetyHead {
                        head: head.to_owned(),
                        probability,
                    });
                }
            }
        }
        Verdict::Allow
    }

    /// Where a click would take the run, when the action carries a
    /// destination.
    fn destination(&self, action: &Action) -> Option<Verdict> {
        let href = action.get("href").and_then(Value::as_str)?;
        let url = url::Url::parse(href).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return Some(Verdict::Refuse(BlockReason::NotWeb {
                scheme: url.scheme().to_owned(),
            }));
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let denied = self.denied_origins.iter().any(|entry| {
            let entry = entry.trim().to_ascii_lowercase();
            !entry.is_empty()
                && (host == entry || host.ends_with(&format!(".{entry}")) || entry == "*")
        });
        denied.then_some(Verdict::Refuse(BlockReason::DeniedOrigin { host }))
    }

    /// Record that the user approved this action, so the retry after their
    /// approval is not stopped by the same rule.
    ///
    /// Single-shot by design: an approval covers the action it was asked
    /// about. Remembered allows are a later decision with a policy table
    /// behind them, not a convenience bolted on here (16 §5.3).
    pub fn approve(&mut self, action: &Action, page_url: &str) {
        if action.get("kind").and_then(Value::as_str) == Some("upload") {
            self.approved_uploads.push(origin_of(action, page_url));
        }
    }

    /// The `on_task` tripwire: two consecutive answers under `floor` end the
    /// run. Anything at or above it forgives the earlier strike.
    ///
    /// A `floor` of zero is the run opting out, and then there is nothing to
    /// strike: `policy::build_request` does not even ask the drift head, and
    /// a head nobody asked for must never read as drift. Above zero the head
    /// was asked, so it was answered or the step already failed — an absent
    /// answer here means nobody wired it, and the unknown scores as drifted
    /// for the same reason the confirm heads fail closed.
    ///
    /// Independent of `safety_heads`: the risk ceilings and the drift floor
    /// are separate questions, and a run may want either without the other.
    pub fn on_task(&mut self, safety: &BTreeMap<String, f64>, floor: f64) -> Option<BlockReason> {
        if floor <= 0.0 {
            return None;
        }
        let answer = safety.get(ON_TASK.0).copied().unwrap_or(0.0);
        if answer >= floor {
            self.off_task_strikes = 0;
            return None;
        }
        self.off_task_strikes += 1;
        (self.off_task_strikes >= MAX_OFF_TASK_STRIKES).then_some(BlockReason::OffTask)
    }
}

/// The first confirm word a label contains, if any.
///
/// Matched on word boundaries so "Pay" trips and "Repayment history" does
/// not; case-insensitive because buttons shout.
pub fn confirm_word(label: &str) -> Option<&'static str> {
    let label = label.to_ascii_lowercase();
    CONFIRM_WORDS.iter().copied().find(|word| {
        label.match_indices(*word).any(|(at, matched)| {
            let before = label[..at].chars().next_back();
            let after = label[at + matched.len()..].chars().next();
            let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            boundary(before) && boundary(after)
        })
    })
}

/// The host an action belongs to, for upload approval: the action's own frame
/// when it came from one, otherwise the page it was observed on.
fn origin_of(action: &Action, page_url: &str) -> String {
    action
        .get("frame")
        .and_then(Value::as_str)
        .into_iter()
        .chain(std::iter::once(page_url))
        .filter_map(|url| url::Url::parse(url).ok())
        .find_map(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "this page".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The page every fixture action was observed on.
    const PAGE: &str = "https://example.test/step";

    fn decision(safety: &[(&str, f64)]) -> Decision {
        Decision {
            operation: "CLICK".into(),
            operation_confidence: 0.9,
            operation_probabilities: BTreeMap::new(),
            action: None,
            target: Some("1".into()),
            target_confidence: Some(0.9),
            safety: safety
                .iter()
                .map(|(head, value)| ((*head).to_owned(), *value))
                .collect(),
        }
    }

    fn action(value: Value) -> Action {
        value.as_object().cloned().unwrap_or_default()
    }

    fn gate() -> Gate {
        Gate::new(0.4, true, vec!["ads.example".into()])
    }

    /// The deterministic half of the safety story: a label that says what it
    /// does is a confirm even when every head scored it harmless. This is the
    /// rule that does not depend on a classifier being right.
    #[test]
    fn a_label_that_spends_confirms_whatever_the_heads_said() {
        let harmless = decision(&[("outward", 0.0), ("destructive", 0.0), ("spends", 0.0)]);
        let pay = action(json!({ "kind": "click", "label": "Pay $42.00 now" }));

        assert_eq!(
            gate().action(&harmless, &pay, PAGE),
            Verdict::Confirm(ConfirmReason::Label { word: "pay" })
        );
    }

    /// …and a label that merely *contains* the letters of one does not, or
    /// every run would be one long confirm. ("Remove filter" genuinely
    /// contains the word and genuinely does confirm — that is the rule
    /// working, not a false positive.)
    #[test]
    fn a_label_that_only_contains_the_letters_of_a_confirm_word_is_allowed() {
        let harmless = decision(&[("outward", 0.0), ("destructive", 0.0), ("spends", 0.0)]);
        for label in ["Repayment history", "Deposits", "Sent items", "Postcode"] {
            let verdict = gate().action(
                &harmless,
                &action(json!({ "kind": "click", "label": label })),
                PAGE,
            );
            assert_eq!(verdict, Verdict::Allow, "`{label}` should not confirm");
        }
    }

    #[test]
    fn a_head_over_the_threshold_confirms_with_its_probability() {
        let risky = decision(&[("outward", 0.0), ("destructive", 0.0), ("spends", 0.81)]);
        let click = action(json!({ "kind": "click", "label": "Continue" }));

        assert_eq!(
            gate().action(&risky, &click, PAGE),
            Verdict::Confirm(ConfirmReason::SafetyHead {
                head: "spends".into(),
                probability: 0.81
            })
        );
    }

    /// The fail-closed rule, at the gate this time: a head nobody answered
    /// scores as risky rather than as safe.
    #[test]
    fn an_absent_head_confirms() {
        let partial = decision(&[("outward", 0.0)]);
        let click = action(json!({ "kind": "click", "label": "Continue" }));

        assert!(matches!(
            gate().action(&partial, &click, PAGE),
            Verdict::Confirm(ConfirmReason::SafetyHead { .. })
        ));
    }

    #[test]
    fn denied_hosts_and_non_web_destinations_are_refused_not_confirmed() {
        let harmless = decision(&[("outward", 0.0), ("destructive", 0.0), ("spends", 0.0)]);
        let mut gate = gate();

        let tracker = action(
            json!({ "kind": "click", "label": "Open", "href": "https://pixel.ads.example/x" }),
        );
        assert_eq!(
            gate.action(&harmless, &tracker, PAGE),
            Verdict::Refuse(BlockReason::DeniedOrigin {
                host: "pixel.ads.example".into()
            })
        );

        let mailto =
            action(json!({ "kind": "click", "label": "Email us", "href": "mailto:a@b.c" }));
        assert_eq!(
            gate.action(&harmless, &mailto, PAGE),
            Verdict::Refuse(BlockReason::NotWeb {
                scheme: "mailto".into()
            })
        );

        let allowed =
            action(json!({ "kind": "click", "label": "Open", "href": "https://example.test/x" }));
        assert_eq!(gate.action(&harmless, &allowed, PAGE), Verdict::Allow);
    }

    /// An upload is approved once per origin per run: the second one does not
    /// ask again, a different host does.
    #[test]
    fn the_first_upload_to_an_origin_confirms_and_the_second_does_not() {
        let harmless = decision(&[("outward", 0.0), ("destructive", 0.0), ("spends", 0.0)]);
        let mut gate = gate();
        let upload =
            action(json!({ "kind": "upload", "label": "Attach", "frame": "https://forms.test/a" }));

        assert_eq!(
            gate.action(&harmless, &upload, PAGE),
            Verdict::Confirm(ConfirmReason::FirstUpload {
                origin: "forms.test".into()
            })
        );
        gate.approve(&upload, PAGE);
        assert_eq!(gate.action(&harmless, &upload, PAGE), Verdict::Allow);

        let elsewhere =
            action(json!({ "kind": "upload", "label": "Attach", "frame": "https://other.test/a" }));
        assert!(matches!(
            gate.action(&harmless, &elsewhere, PAGE),
            Verdict::Confirm(ConfirmReason::FirstUpload { .. })
        ));
    }

    /// Waits and scrolls are not decisions anybody approves; asking would
    /// make the gate the thing users fight.
    #[test]
    fn waiting_and_scrolling_never_confirm() {
        let risky = decision(&[("outward", 0.9), ("destructive", 0.9), ("spends", 0.9)]);
        let mut gate = gate();

        for kind in ["wait", "scroll"] {
            let verdict = gate.action(
                &risky,
                &action(json!({ "kind": kind, "label": "Wait" })),
                PAGE,
            );
            assert_eq!(verdict, Verdict::Allow);
        }
    }

    #[test]
    fn a_password_field_or_a_challenge_hands_the_page_over() {
        let gate = gate();
        let login = json!({ "signals": { "password_fields": 1, "captcha": 0 } });
        let challenge = json!({ "signals": { "password_fields": 0, "captcha": 1 } });
        let ordinary = json!({ "signals": { "password_fields": 0, "captcha": 0 } });

        assert_eq!(gate.page(&login), Some(NeedsUser::SignIn));
        assert_eq!(gate.page(&challenge), Some(NeedsUser::Captcha));
        assert_eq!(gate.page(&ordinary), None);
    }

    /// One irrelevant page is normal; two in a row is a run that has
    /// wandered, or page text steering the classifier.
    #[test]
    fn off_task_needs_two_strikes_and_forgives_a_good_answer() {
        let mut gate = gate();
        let on = BTreeMap::from([("on_task".to_owned(), 0.9)]);
        let off = BTreeMap::from([("on_task".to_owned(), 0.1)]);

        assert_eq!(gate.on_task(&off, ON_TASK_FLOOR), None);
        assert_eq!(
            gate.on_task(&on, ON_TASK_FLOOR),
            None,
            "a good answer clears the strike"
        );
        assert_eq!(gate.on_task(&off, ON_TASK_FLOOR), None);
        assert_eq!(
            gate.on_task(&off, ON_TASK_FLOOR),
            Some(BlockReason::OffTask)
        );
    }

    /// A run with the floor at zero never asked the drift head, so nothing
    /// answered it — and an unanswered question must not end the run. This is
    /// the one reading that would make the setting unusable.
    #[test]
    fn a_floor_of_zero_never_strikes() {
        let mut gate = gate();
        let nothing = BTreeMap::new();

        for _ in 0..MAX_OFF_TASK_STRIKES + 1 {
            assert_eq!(gate.on_task(&nothing, 0.0), None);
        }
    }
}
