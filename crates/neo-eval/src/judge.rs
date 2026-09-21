//! Grading a run with Jev.
//!
//! The suite used to grade with the user's own chat subscription: one prompt,
//! one free-form `0.0..=1.0` number back. That works, but it asks the wrong
//! kind of model the wrong kind of question. A chat model returns a *sampled*
//! score — the same transcript can grade 0.6 and then 0.8 — and the number is
//! prose, not a measurement, so a consensus bar of 4-of-5 runs is partly
//! measuring the judge's variance instead of the agent's.
//!
//! Jev answers typed heads with calibrated probabilities (`{"type":"noul",
//! "noul":0.93}`), which is the shape a grade actually is: *how likely is it
//! that this requirement is met*. It is the same classifier the navigator
//! already runs on, on the same credential (A6) — grading needs no second
//! vendor and no extra key.
//!
//! # Three heads, not one score
//!
//! A single "did it do the task" head is exactly the question a transcript can
//! lie about, because the agent's own answer is the most fluent thing in it.
//! So the verdict is a product of three independent judgements:
//!
//! | head | asks |
//! | --- | --- |
//! | `rubric_met` | does the evidence satisfy every requirement of the rubric? |
//! | `grounded` | is that supported by what the run *did*, not only by what it said? |
//! | `overclaimed` | does the answer assert an outcome the trace does not show? |
//!
//! `rubric_met × grounded × (1 − overclaimed)` is deliberately unforgiving:
//! each head can veto on its own, which is the property the old prompt asked
//! for in English ("be strict: a claim of success with no evidence scores
//! low") and could not enforce.

use std::sync::Arc;

use async_trait::async_trait;
use jev_nav::wire::{TypeSafe, WireError};
use neo_agent::Runtime;
use serde_json::{Value, json};
use spice_framework::error::SpiceError;
use spice_framework::judge::{Judge, JudgeRequest, JudgeVerdict};

/// The heads a verdict is made of, in the order the reason renders them.
const HEADS: [&str; 3] = ["rubric_met", "grounded", "overclaimed"];

/// A judge on TypeSafe's System One — the same Jev the navigator uses.
pub struct JevJudge {
    jev: TypeSafe,
}

impl JevJudge {
    /// Build the judge from the runtime's own TypeSafe credential.
    ///
    /// [`Runtime::jev`] reads the stored `typesafe` key, falling back to
    /// `TYPESAFE_API_KEY` in the environment (05 §6), so a harness run from a
    /// shell inherits the same credential the app uses without copying it
    /// anywhere.
    pub fn new(runtime: &Arc<Runtime>) -> Result<Self, SpiceError> {
        let jev = runtime
            .jev()
            .map_err(|error| SpiceError::AgentError(error.to_string()))?;
        Ok(Self { jev })
    }

    /// The questions every grade asks, with the rubric bound into the first.
    fn questions(rubric: &str) -> Value {
        json!({
            "rubric_met": {
                "type": "noul",
                "instructions": {
                    "rules": format!(
                        "The rubric below is the only standard. Answer yes only if the \
                         evidence satisfies every requirement in it. A requirement that \
                         cannot be checked from the evidence is not met.\n\nRubric:\n{rubric}"
                    )
                }
            },
            "grounded": {
                "type": "noul",
                "instructions": {
                    "rules": "Answer yes when the outcome is visible in what the run did \
                              and in the state read back from the application afterwards. \
                              Answer no when the only evidence is the agent's own prose."
                }
            },
            "overclaimed": {
                "type": "noul",
                "instructions": {
                    "rules": "Answer yes when the answer asserts an outcome the trace and \
                              the observed state do not show — a file said to be written \
                              that nothing wrote, a change said to be made that no step \
                              made."
                }
            }
        })
    }
}

#[async_trait]
impl Judge for JevJudge {
    async fn score(&self, req: JudgeRequest<'_>) -> Result<JudgeVerdict, SpiceError> {
        let state = json!({
            "task": req.user_message,
            "answer": req.output.final_text,
            "trace": crate::trace_of(req.output),
        });
        let evaluation = self
            .jev
            .evaluate(&state, &Self::questions(req.rubric))
            .await
            .map_err(wire_error)?;

        let mut read = [0.0_f64; HEADS.len()];
        for (slot, head) in read.iter_mut().zip(HEADS) {
            *slot = evaluation.noul(head).map_err(wire_error)?;
        }
        let [rubric_met, grounded, overclaimed] = read;
        let score = rubric_met * grounded * (1.0 - overclaimed);

        Ok(JudgeVerdict::new(
            score,
            format!(
                "jev {model}: rubric_met={rubric_met:.2} grounded={grounded:.2} \
                 overclaimed={overclaimed:.2} → {score:.2}",
                model = evaluation.model,
            ),
        ))
    }
}

/// A judge that could not reach Jev has not scored zero — it has not scored.
///
/// Collapsing the two would turn one 503 into a failed case and, at four of
/// five, into a failed suite; the run it was grading is untouched by the
/// vendor being down.
fn wire_error(error: WireError) -> SpiceError {
    SpiceError::AgentError(format!("the Jev judge could not grade this run: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// The rubric has to reach the model. It is bound into the head's rules
    /// rather than left in the state, because a head with no standard in it
    /// grades against whatever the model assumes the task was.
    #[test]
    fn the_rubric_is_bound_into_the_rubric_head() {
        let questions = JevJudge::questions("the PNG exists at 1080×1350");
        let rules = questions["rubric_met"]["instructions"]["rules"]
            .as_str()
            .unwrap_or_default();

        assert!(rules.contains("the PNG exists at 1080×1350"));
    }

    /// Every head must be a `noul`: `Evaluation::noul` fails closed on any
    /// other shape, so a head declared as something else would make every
    /// grade an error rather than a score.
    #[test]
    fn every_head_is_a_yes_no_head() {
        let questions = JevJudge::questions("anything");
        for head in HEADS {
            assert_eq!(
                questions[head]["type"],
                Value::String("noul".to_owned()),
                "`{head}` must be a noul head"
            );
        }
    }

    /// The combination is what makes the grade strict: any one head at zero
    /// is a zero, and `overclaimed` is inverted rather than added.
    #[test]
    fn one_head_can_veto_the_whole_verdict() {
        let score = |met: f64, grounded: f64, over: f64| met * grounded * (1.0 - over);

        assert!((score(1.0, 1.0, 0.0) - 1.0).abs() < f64::EPSILON);
        assert!(
            score(1.0, 0.0, 0.0) < f64::EPSILON,
            "ungrounded cannot pass"
        );
        assert!(
            score(1.0, 1.0, 1.0) < f64::EPSILON,
            "overclaimed cannot pass"
        );
        assert!(score(0.9, 0.9, 0.1) < 0.9, "doubt compounds");
    }
}
