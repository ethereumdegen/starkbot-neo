//! The suite: what "Starkbot can operate this app" means, case by case.
//!
//! Each case is one concrete outcome a GTM user would actually ask for, and
//! each is scored against the **application's state** through a probe, never
//! against the model's prose. A case names the [`App`] it needs, so a machine
//! without Diffusion Studio skips that case with a reason instead of failing.
//!
//! Nondeterminism is handled the way the plan already specified for S8a:
//! `consensus_runs(5)` with `consensus_required(4)`. A UI-driving agent that
//! passes once out of five is not working, and one that fails once out of five
//! is not broken.

use serde_json::json;
use spice_framework::assertion::Assertion;
use spice_framework::test_case::{TestCase, TestSuite};

use crate::apps::App;
use crate::fixture::Fixture;
use crate::probe::Probe;

/// One eval case plus the app it needs.
pub struct Case {
    pub app: App,
    pub test: TestCase,
}

/// The suite's name, as it appears on every report. A report merged from
/// per-case runs has to name the same suite the runner would have.
pub const SUITE_NAME: &str = "app control";

/// Runs that must pass for a case to pass, out of [`CONSENSUS_RUNS`].
pub const CONSENSUS_REQUIRED: usize = 4;
/// How many times each case runs (S8a).
pub const CONSENSUS_RUNS: usize = 5;

/// Every case, in rising order of difficulty.
#[must_use]
pub fn all() -> Vec<Case> {
    vec![
        read_a_page(),
        extract_from_a_page(),
        type_into_textedit(),
        format_in_textedit(),
        write_a_calc_cell(),
        read_a_calc_cell(),
        spreadsheet_surface_offers_menus(),
        read_a_numbers_cell(),
        numbers_surface_offers_menus(),
        open_diffusion_studio(),
    ]
}

/// One case per `TestSuite`, or the whole set: the runner is driven a case at
/// a time so progress can be published between them ([`crate::run_suite`]).
#[must_use]
pub fn suite(cases: Vec<Case>) -> TestSuite {
    TestSuite {
        name: SUITE_NAME.to_owned(),
        tests: cases.into_iter().map(|case| case.test).collect(),
        ..Default::default()
    }
}

fn case(
    app: App,
    id: &str,
    message: &str,
    probe: Probe,
    assertions: Vec<Assertion>,
    tags: &[&str],
) -> Case {
    case_with(app, id, message, probe, None, assertions, tags)
}

/// A case with a fixture: the app is put into a known state first.
fn case_with(
    app: App,
    id: &str,
    message: &str,
    probe: Probe,
    fixture: Option<Fixture>,
    assertions: Vec<Assertion>,
    tags: &[&str],
) -> Case {
    let mut data = probe.config();
    if let (Some(fixture), Some(object)) = (fixture, data.as_object_mut())
        && let Some(entry) = fixture.config().as_object().and_then(|f| f.get("fixture"))
    {
        object.insert("fixture".to_owned(), entry.clone());
    }
    Case {
        app,
        test: TestCase {
            id: id.to_owned(),
            name: Some(format!("{}: {id}", app.label())),
            user_message: message.to_owned(),
            config: spice_framework::agent::AgentConfig { data },
            assertions,
            judges: vec![],
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            retries: 0,
            consensus_runs: Some(CONSENSUS_RUNS),
            consensus_required: Some(CONSENSUS_REQUIRED),
            timeout: Some(std::time::Duration::from_secs(180)),
        },
    }
}

// ------------------------------------------------------------------ browser

/// The floor: open a page and report what is on it. If this fails, nothing
/// else in the suite can be trusted.
fn read_a_page() -> Case {
    case(
        App::Chrome,
        "read-a-page",
        "Open https://example.com and tell me the exact heading text on the page.",
        Probe::AppText {
            app: App::Chrome.selector().to_owned(),
        },
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            Assertion::ExpectToolsWithinAllowlist,
            // The answer has to contain the heading that is really there.
            Assertion::ExpectTextContains("Example Domain".to_owned()),
            // And the page has to actually be open — a model that answers
            // from memory without browsing fails here.
            Assertion::ExpectToolArgExists("browse".to_owned(), "url".to_owned()),
        ],
        &["browser", "smoke"],
    )
}

/// Reading a specific fact off a page, which is the shape of most GTM
/// research work.
fn extract_from_a_page() -> Case {
    case(
        App::Chrome,
        "extract-from-a-page",
        "Go to https://www.rfc-editor.org/rfc/rfc2119.txt and tell me the RFC number \
         and the single word this RFC says is an absolute requirement.",
        Probe::AppText {
            app: App::Chrome.selector().to_owned(),
        },
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            Assertion::ExpectTextContains("2119".to_owned()),
            Assertion::ExpectTextContains("MUST".to_owned()),
        ],
        &["browser", "extract"],
    )
}

// ------------------------------------------------------------------ TextEdit

/// A23's canonical target: type into a document that ships with the OS.
///
/// The probe reads the document back, so "I typed it" is not enough.
fn type_into_textedit() -> Case {
    case_with(
        App::TextEdit,
        "type-into-textedit",
        "In TextEdit, type exactly: Q4 launch brief",
        Probe::AppText {
            app: App::TextEdit.selector().to_owned(),
        },
        // A fresh document, or the probe reads whatever was left on screen.
        Some(Fixture::TextDocument {
            app: App::TextEdit.selector().to_owned(),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["app".to_owned()]),
            Assertion::ExpectToolArg("app".to_owned(), "app".to_owned(), json!("TextEdit")),
            // The document itself contains the text.
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                let text = probe
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if text.contains("Q4 launch brief") {
                    Ok(())
                } else {
                    Err(format!(
                        "the document does not contain the text; it holds: {:?}",
                        text.chars().take(120).collect::<String>()
                    ))
                }
            })),
        ],
        &["app", "smoke", "a23"],
    )
}

/// Using a menu rather than typing — the other half of A23's smoke test.
fn format_in_textedit() -> Case {
    case_with(
        App::TextEdit,
        "format-in-textedit",
        "In TextEdit, turn on bold using the formatting controls.",
        Probe::Surface {
            app: App::TextEdit.selector().to_owned(),
        },
        Some(Fixture::TextDocument {
            app: App::TextEdit.selector().to_owned(),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["app".to_owned()]),
            // The outcome, not a proxy. An earlier version of this case
            // budgeted the agent's actions and failed a run that turned bold
            // on in four steps — which is the wrong thing to measure: a case
            // that counts steps passes an agent that did nothing quickly.
            // `engaged` is every toggle the window reports as on.
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                let engaged = probe
                    .get("engaged")
                    .and_then(|value| value.as_array())
                    .map(|labels| {
                        labels
                            .iter()
                            .filter_map(|label| label.as_str())
                            .map(str::to_lowercase)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if engaged.iter().any(|label| label.contains("bold")) {
                    Ok(())
                } else {
                    Err(format!("bold is not on; engaged: {engaged:?}"))
                }
            })),
        ],
        &["app", "a23"],
    )
}

// ---------------------------------------------------------------- spreadsheet

/// The case that is currently **expected to fail**, and is in the suite for
/// exactly that reason.
///
/// Writing a *named* cell needs the grid selection to move, and neither
/// `AXFocused` nor `AXPress` moves LibreOffice's selection (01
/// §spreadsheets). Until that is solved the agent can only type into whatever
/// cell is already selected. A failing case with a probe that says *which*
/// cell got the value is the measurement that tells us when the fix lands.
fn write_a_calc_cell() -> Case {
    case_with(
        App::LibreOffice,
        "write-a-calc-cell",
        "In LibreOffice Calc, put the number 42 in cell B2.",
        crate::probe::cell_of(App::LibreOffice, "B2"),
        Some(Fixture::Spreadsheet {
            app: App::LibreOffice.selector().to_owned(),
            a1: "Starkbot 42".to_owned(),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["app".to_owned()]),
            Assertion::ExpectToolArg("probe".to_owned(), "found".to_owned(), json!(true)),
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                match probe.get("value").and_then(|value| value.as_str()) {
                    Some(value) if value.contains("42") => Ok(()),
                    Some(other) => Err(format!("B2 holds {other:?}, not 42")),
                    None => Err("B2 is empty".to_owned()),
                }
            })),
        ],
        &["spreadsheet", "a23", "known-gap"],
    )
}

/// Reading a cell is the half that already works, and it is worth pinning
/// separately: it proves the grid is legible even while writing to a named
/// cell is not.
fn read_a_calc_cell() -> Case {
    case_with(
        App::LibreOffice,
        "read-a-calc-cell",
        "In LibreOffice Calc, tell me what is in cell A1.",
        crate::probe::cell_of(App::LibreOffice, "A1"),
        Some(Fixture::Spreadsheet {
            app: App::LibreOffice.selector().to_owned(),
            a1: "Starkbot 42".to_owned(),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["app".to_owned()]),
            // The cell has to be *offered* to the model. On a fresh sheet it
            // is empty, so `found` would be false — what this case measures is
            // that the grid is legible at all, which is the half of A23 that
            // works.
            Assertion::ExpectToolArg("probe".to_owned(), "present".to_owned(), json!(true)),
            // The fixture put this there, so the agent has something real to
            // read and the assertion cannot pass by accident.
            Assertion::ExpectToolArg("probe".to_owned(), "value".to_owned(), json!("Starkbot 42")),
            Assertion::ExpectTextContains("Starkbot 42".to_owned()),
        ],
        &["spreadsheet", "a23"],
    )
}

/// The pruning question A23 leaves open, as a measurement rather than a note:
/// on a spreadsheet the 250-row budget is consumed by grid cells, so the
/// app's own menus never reach the model. This case fails until pruning
/// reserves room for them.
fn spreadsheet_surface_offers_menus() -> Case {
    case_with(
        App::LibreOffice,
        "spreadsheet-offers-menus",
        "In LibreOffice Calc, tell me which menus are available.",
        Probe::Surface {
            app: App::LibreOffice.selector().to_owned(),
        },
        Some(Fixture::Spreadsheet {
            app: App::LibreOffice.selector().to_owned(),
            a1: "Starkbot 42".to_owned(),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                let menus = probe
                    .get("menus")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if menus >= 5 {
                    Ok(())
                } else {
                    Err(format!(
                        "only {menus} menu element(s) in the table — the grid is still \
                         crowding out the app's own menus (A23 pruning)"
                    ))
                }
            })),
        ],
        &["spreadsheet", "a23", "known-gap"],
    )
}

/// The same spreadsheet question on a second app.
///
/// This is what tells us whether the path is generic (A23: one accessibility
/// policy, no per-app profile) or whether LibreOffice's behaviour was
/// LibreOffice's. Numbers ships on every Mac — on this machine as
/// *Numbers Creator Studio.app*, which is why app lookup searches name
/// variants.
fn read_a_numbers_cell() -> Case {
    case_with(
        App::Numbers,
        "read-a-numbers-cell",
        "In Numbers, tell me what is in the first cell of the table.",
        crate::probe::cell_of(App::Numbers, "A1"),
        // A generated CSV, so the cell has a value the assertion can name.
        // Numbers imports CSV without a dialog; it has no flat-ODF importer,
        // which is why the fixture picks the format per app.
        Some(Fixture::Spreadsheet {
            app: App::Numbers.selector().to_owned(),
            a1: "Starkbot 42".to_owned(),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["app".to_owned()]),
            Assertion::ExpectToolsWithinAllowlist,
            // The probe is the ground truth; the model's prose is not.
            // Numbers has no cell addresses, so the assertion is that the
            // fixture's value is in the grid the model was shown.
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                let grid = probe
                    .get("grid")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if grid.contains("Starkbot 42") {
                    Ok(())
                } else {
                    Err(format!(
                        "the grid does not hold the fixture's value: {grid:.160}"
                    ))
                }
            })),
        ],
        &["spreadsheet", "numbers", "a23"],
    )
}

/// Whether Numbers' own menus survive the element budget, which is the
/// pruning question A23 asks — measured on a second app so the answer is not
/// one app's quirk.
fn numbers_surface_offers_menus() -> Case {
    case_with(
        App::Numbers,
        "numbers-offers-menus",
        "In Numbers, tell me which menus are available.",
        Probe::Surface {
            app: App::Numbers.selector().to_owned(),
        },
        Some(Fixture::Spreadsheet {
            app: App::Numbers.selector().to_owned(),
            a1: "Starkbot 42".to_owned(),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                let menus = probe
                    .get("menus")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if menus >= 5 {
                    Ok(())
                } else {
                    Err(format!("only {menus} menu element(s) in the table"))
                }
            })),
        ],
        &["spreadsheet", "numbers", "a23"],
    )
}

// ----------------------------------------------------------------- media apps

/// The media path of A12′/S8a. Skipped on a machine without the app; the
/// assertion is deliberately modest, because the first thing to establish is
/// that a media app's window is legible at all.
fn open_diffusion_studio() -> Case {
    case(
        App::DiffusionStudio,
        "open-diffusion-studio",
        "Open Diffusion Studio and tell me what the window offers.",
        Probe::Surface {
            app: App::DiffusionStudio.selector().to_owned(),
        },
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["app".to_owned()]),
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                let rows = probe
                    .get("rows")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                if rows >= 5 {
                    Ok(())
                } else {
                    Err(format!(
                        "only {rows} element(s) — an Electron window usually needs \
                         AXManualAccessibility (A19) before it is legible"
                    ))
                }
            })),
        ],
        &["media", "s8a"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every case runs five times and needs four passes: the bar S8a set for
    /// UI automation. A case with `retries` instead would hide flakiness.
    #[test]
    fn every_case_uses_consensus_rather_than_retries() {
        for case in all() {
            assert_eq!(
                case.test.consensus_runs,
                Some(CONSENSUS_RUNS),
                "{} must run {CONSENSUS_RUNS} times",
                case.test.id
            );
            assert_eq!(case.test.consensus_required, Some(CONSENSUS_REQUIRED));
            assert_eq!(case.test.retries, 0, "{} must not retry", case.test.id);
        }
    }

    /// A case that cannot check the app's state is not an eval. Every case
    /// declares a probe.
    #[test]
    fn every_case_probes_the_application() {
        for case in all() {
            assert!(
                Probe::from_config(&case.test.config).is_some(),
                "{} has no probe, so it can only grade prose",
                case.test.id
            );
        }
    }

    /// The two cases that are expected to fail today are tagged, so a report
    /// can tell a regression from a known gap.
    #[test]
    fn the_known_gaps_are_tagged() {
        let gaps: Vec<String> = all()
            .into_iter()
            .filter(|case| case.test.tags.iter().any(|tag| tag == "known-gap"))
            .map(|case| case.test.id)
            .collect();
        assert!(gaps.contains(&"write-a-calc-cell".to_owned()));
        assert!(gaps.contains(&"spreadsheet-offers-menus".to_owned()));
    }

    /// Every case that drives a document app starts it from a known state.
    /// Without this the suite measures the machine's history: the first
    /// TextEdit run was scored against a document an earlier session had left
    /// open.
    #[test]
    fn document_cases_declare_a_fixture() {
        for case in all() {
            if matches!(case.app, App::TextEdit | App::LibreOffice | App::Numbers)
                && case.test.tags.iter().all(|tag| tag != "known-gap")
            {
                assert!(
                    Fixture::from_config(&case.test.config).is_some(),
                    "{} drives a document app with no fixture",
                    case.test.id
                );
            }
        }
    }
}
