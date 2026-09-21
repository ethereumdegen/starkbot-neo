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

use serde_json::{Value, json};
use spice_framework::agent::AgentOutput;
use spice_framework::assertion::Assertion;
use spice_framework::test_case::{TestCase, TestSuite};

use crate::apps::App;
use crate::cards::Cards;
use crate::fixture::Fixture;
use crate::pages;
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
    let mut cases = vec![
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
    ];
    cases.extend(review_set());
    cases.extend(live_review_set());
    cases
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
    build(app, id, message, probe, None, None, assertions, tags)
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
    build(app, id, message, probe, fixture, None, assertions, tags)
}

/// One case, however much of the harness it needs.
///
/// Every `TestCase` in the suite is built here, so the consensus bar, the
/// timeout and the `<app>: <id>` name cannot differ between one family of
/// cases and another — the filter a front end shows is written against that
/// name.
#[allow(clippy::too_many_arguments)]
fn build(
    app: App,
    id: &str,
    message: &str,
    probe: Probe,
    fixture: Option<Fixture>,
    cards: Option<Cards>,
    assertions: Vec<Assertion>,
    tags: &[&str],
) -> Case {
    let mut data = probe.config();
    for fragment in [
        fixture.map(|fixture| ("fixture", fixture.config())),
        cards.map(|cards| ("cards", cards.config())),
    ]
    .into_iter()
    .flatten()
    {
        let (key, value) = fragment;
        if let (Some(object), Some(entry)) = (
            data.as_object_mut(),
            value.as_object().and_then(|f| f.get(key)),
        ) {
            object.insert(key.to_owned(), entry.clone());
        }
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
            timeout: Some(crate::CASE_TIMEOUT),
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

// ------------------------------------------------- the navigation review set

/// The tag the release gate runs: the fifteen fixed navigation tasks of
/// bar B7 (16 §0, §6.2).
///
/// Fifteen, fixed, and all fifteen on the repo's own pages — a gate whose
/// pass rate moves with somebody else's uptime is not a gate. Adding a case
/// to this tag changes what a tag means, so the count is pinned by a test.
pub const REVIEW_TAG: &str = "nav-review";

/// The three real-site cases. A separate tag rather than an extra tag on the
/// review set, because [`crate::Selection`] composes tags as *any-of* and has
/// no way to exclude one: if these carried [`REVIEW_TAG`] there would be no
/// command that runs only the deterministic fifteen.
pub const LIVE_TAG: &str = "nav-review-live";

/// The fifteen, in the order the plan lists the shapes they cover.
fn review_set() -> Vec<Case> {
    vec![
        fill_and_save_a_form(),
        click_inside_a_cross_origin_frame(),
        fill_a_shadow_dom_field(),
        attach_a_file(),
        follow_a_popup(),
        scroll_a_nested_list(),
        choose_from_a_dropdown(),
        press_a_key(),
        choose_an_autocomplete_suggestion(),
        search_then_open_a_result(),
        refuse_to_call_an_animation_progress(),
        pay_only_with_an_approval(),
        resume_after_a_login_wall(),
        refuse_a_non_web_destination(),
        answer_a_fact_off_the_page(),
    ]
}

/// The three that need the network, a live model and — for the last one — a
/// profile a human has signed into. Excluded from the gate's own command.
fn live_review_set() -> Vec<Case> {
    vec![
        search_and_open_on_a_real_site(),
        read_a_fact_off_a_real_site(),
        work_behind_a_login_the_user_performed(),
    ]
}

/// One review-set case: a fixture page, its record cleared first, its record
/// scored afterwards.
fn nav_case(
    id: &str,
    message: String,
    cards: Option<Cards>,
    assertions: Vec<Assertion>,
    tags: &[&str],
) -> Case {
    build(
        App::Chrome,
        id,
        &message,
        Probe::Page {
            app: App::Chrome.selector().to_owned(),
            url: pages::state_url(),
        },
        Some(Fixture::WebPage {
            app: App::Chrome.selector().to_owned(),
            url: pages::state_url(),
        }),
        cards,
        assertions,
        tags,
    )
}

/// The message a review-set case sends: one page, one outcome.
fn goal(page: &str, outcome: &str) -> String {
    format!("Open {} and {outcome}", pages::url(page))
}

/// What the page recorded, or why there is nothing to score.
///
/// A probe that could not read the profile back reports its error, and that
/// has to fail the case *as a harness fault*, not as an empty page: the two
/// are indistinguishable in a bare `{}`.
fn recorded(output: &AgentOutput) -> Result<Value, String> {
    let probe = output
        .tool_calls_by_name("probe")
        .first()
        .map(|call| call.arguments.clone())
        .ok_or("no probe ran")?;
    if let Some(error) = probe.get("error").and_then(Value::as_str) {
        return Err(format!("the page could not be read back: {error}"));
    }
    Ok(probe.get("state").cloned().unwrap_or(Value::Null))
}

/// The cards the run published and what the person watching answered.
fn played(output: &AgentOutput) -> Result<Value, String> {
    output
        .tool_calls_by_name("cards")
        .first()
        .map(|call| call.arguments.clone())
        .ok_or_else(|| "the run published no cards at all".to_owned())
}

/// Everything the navigator reported about its own runs, as it reported it to
/// the model: `Done after 2 action(s)`, `Blocked after 3 action(s): …`.
///
/// This is a published `AppEvent::TurnStepDone`, not the model's prose — the
/// distinction matters, because a case about *how a run ended* cannot be
/// scored on what the model said about it afterwards.
fn navigator_said(output: &AgentOutput) -> String {
    output
        .turns
        .iter()
        .filter(|turn| {
            turn.tool_calls
                .iter()
                .any(|call| call.name == "browse" || call.name == "app")
        })
        .filter_map(|turn| turn.output_text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One value out of a nested record: `request.name`.
fn dig<'a>(record: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(record, |value, key| value.get(key))
}

/// The page recorded exactly this at this path.
fn records(path: &'static str, expected: Value) -> Assertion {
    Assertion::Custom(Box::new(move |output| {
        let record = recorded(output)?;
        match dig(&record, path) {
            Some(found) if *found == expected => Ok(()),
            Some(found) => Err(format!(
                "the page recorded {path} as {found}, not {expected}; \
                 its whole record is {record}"
            )),
            None => Err(format!(
                "the page never recorded {path}; its whole record is {record}"
            )),
        }
    }))
}

/// The page recorded something containing this at this path. For values a
/// person would write differently than the page stores them.
fn records_containing(path: &'static str, expected: &'static str) -> Assertion {
    Assertion::Custom(Box::new(move |output| {
        let record = recorded(output)?;
        let found = dig(&record, path).and_then(Value::as_str).unwrap_or("");
        if found.to_lowercase().contains(&expected.to_lowercase()) {
            Ok(())
        } else {
            Err(format!(
                "the page recorded {path} as {found:?}, which does not contain \
                 {expected:?}; its whole record is {record}"
            ))
        }
    }))
}

/// The page recorded nothing at all: nothing was done to it.
fn records_nothing() -> Assertion {
    Assertion::Custom(Box::new(|output| {
        let record = recorded(output)?;
        match record.as_object().map(serde_json::Map::is_empty) {
            Some(true) | None => Ok(()),
            Some(false) => Err(format!(
                "the page was changed when nothing should have reached it: {record}"
            )),
        }
    }))
}

/// A confirm card was published for this rule, and answering it this way is
/// what the run went on with.
fn confirm_card(cause: &'static str, answered: &'static str) -> Assertion {
    Assertion::Custom(Box::new(move |output| {
        let cards = played(output)?;
        let confirms = cards["confirms"].as_array().cloned().unwrap_or_default();
        let matched = confirms.iter().any(|card| {
            card["cause"]
                .as_str()
                .is_some_and(|actual| actual.starts_with(cause))
                && card["answered"].as_str() == Some(answered)
                && card["delivered"] == json!(true)
        });
        if matched {
            Ok(())
        } else {
            Err(format!(
                "no confirm card for `{cause}` was published and {answered}; \
                 the run published {cards}"
            ))
        }
    }))
}

/// A question was published carrying this sentence, and it was answered.
fn ask_card(fragment: &'static str) -> Assertion {
    Assertion::Custom(Box::new(move |output| {
        let cards = played(output)?;
        let asks = cards["asks"].as_array().cloned().unwrap_or_default();
        let matched = asks.iter().any(|card| {
            card["question"]
                .as_str()
                .is_some_and(|question| question.contains(fragment))
                && card["delivered"] == json!(true)
        });
        if matched {
            Ok(())
        } else {
            Err(format!(
                "the run never asked about {fragment:?}; it published {cards}"
            ))
        }
    }))
}

/// No card at all: whatever stopped this run, it was not something a person
/// could have waved through.
fn no_cards() -> Assertion {
    Assertion::Custom(Box::new(|output| {
        let cards = played(output)?;
        if cards["count"] == json!(0) {
            Ok(())
        } else {
            Err(format!(
                "this run must not be answerable by a person, but it published {cards}"
            ))
        }
    }))
}

/// The navigator stopped rather than claiming the work was done.
fn navigator_stopped(reason: &'static str) -> Assertion {
    Assertion::Custom(Box::new(move |output| {
        let said = navigator_said(output);
        if said.contains("Done after") {
            return Err(format!(
                "the navigator reported the goal done on a page that cannot do it: {said:.400}"
            ));
        }
        if said.contains(reason) {
            Ok(())
        } else {
            Err(format!(
                "the navigator did not stop with {reason:?}; it said: {said:.400}"
            ))
        }
    }))
}

/// Where the run ended, as the page itself reported it — the navigator reads
/// `location.href` and `document.title` back out of the tab before it lets
/// go, and that is what a live case can be scored on.
fn ended_at(fragment: &'static str) -> Assertion {
    Assertion::Custom(Box::new(move |output| {
        let said = navigator_said(output);
        if said.contains(fragment) {
            Ok(())
        } else {
            Err(format!(
                "the run never ended at {fragment:?}; the navigator said: {said:.400}"
            ))
        }
    }))
}

/// Chrome's window, as accessibility sees it: the title is the page's own
/// title, which is what a live case scores instead of a fixture record.
fn window_shows(fragment: &'static str) -> Assertion {
    Assertion::Custom(Box::new(move |output| {
        let probe = output
            .tool_calls_by_name("probe")
            .first()
            .map(|call| call.arguments.clone())
            .ok_or("no probe ran")?;
        let window = probe
            .get("window")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if window.to_lowercase().contains(&fragment.to_lowercase()) {
            Ok(())
        } else {
            Err(format!(
                "Chrome's window is {window:?}, which does not show {fragment:?}"
            ))
        }
    }))
}

/// The floor of the web half: two fields and a button, and the form holds
/// what it was told to hold. Everything else in the review set assumes this
/// works.
fn fill_and_save_a_form() -> Case {
    nav_case(
        "nav-form-submit",
        goal(
            "nav-form.html",
            "save an access request for Ada Lovelace at ada@starkbot.test.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            Assertion::ExpectToolsWithinAllowlist,
            records("request.name", json!("Ada Lovelace")),
            records("request.email", json!("ada@starkbot.test")),
        ],
        &[REVIEW_TAG, "browser", "forms"],
    )
}

/// An out-of-process frame. The button is served by the other host name of
/// the fixture server, so Chrome isolates it into its own process and the
/// snapshot has to reach it through a per-frame isolated world — the thing
/// the parity spike proves by hand.
fn click_inside_a_cross_origin_frame() -> Case {
    nav_case(
        "nav-cross-origin-frame",
        goal(
            "nav-crossorigin.html",
            "acknowledge delivery notice 8841 in the embedded panel.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            // The parent records what the frame posted to it, so this can only
            // be true if the click landed inside the frame.
            records("notice.state", json!("acknowledged")),
            records("notice.id", json!(8841)),
        ],
        &[REVIEW_TAG, "browser", "frames"],
    )
}

/// A field inside an open shadow root: invisible to a light-DOM query and to
/// an accessibility read of Chrome's window, which is why this case is scored
/// on the component's own record.
fn fill_a_shadow_dom_field() -> Case {
    nav_case(
        "nav-shadow-field",
        goal("nav-shadow.html", "store the display name Nova Vega."),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            records("display_name", json!("Nova Vega")),
        ],
        &[REVIEW_TAG, "browser", "shadow-dom"],
    )
}

/// The upload gate: the first upload to an origin is a confirm (10 §11.3),
/// and an approval has to finish the job.
///
/// **Expected to fail today**, and in the set for that reason. An agent turn
/// builds its browser run with `BrowserOptions::unattended`, which carries no
/// attachments (`neo-agent/src/agent/metal.rs`), and the web observer removes
/// every upload action when there is nothing to attach
/// (`jev-nav/src/web.rs`). So the agent cannot upload at all: no card is
/// published and the page records nothing. The measurement is what tells us
/// when an attachment reaches the tool — the case is the definition of done
/// for that work, not a guess at it.
fn attach_a_file() -> Case {
    nav_case(
        "nav-upload-confirm",
        goal(
            "nav-upload.html",
            "attach the file upload.txt as the campaign asset.",
        ),
        Some(Cards::approving()),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            confirm_card("upload:", "confirmed"),
            records("upload.name", json!("upload.txt")),
        ],
        &[REVIEW_TAG, "browser", "upload", "known-gap"],
    )
}

/// A new tab the page opened. The only control that can finish the goal is in
/// the popup, so a run that never adopted it cannot pass — and the record is
/// written by the popup, which shares this origin's storage.
fn follow_a_popup() -> Case {
    nav_case(
        "nav-popup-adopt",
        goal("nav-popup.html", "mark receipt 5120 read."),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            records("receipt.state", json!("read")),
            records("receipt.id", json!(5120)),
        ],
        &[REVIEW_TAG, "browser", "popup"],
    )
}

/// A control inside a list's own scrollport. The page itself does not scroll,
/// so only the nested-scroll action reaches the button.
fn scroll_a_nested_list() -> Case {
    nav_case(
        "nav-nested-scroll",
        goal(
            "nav-scroll.html",
            "pick pallet 1012 from the warehouse rows.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            records("picked.pallet", json!(1012)),
        ],
        &[REVIEW_TAG, "browser", "scroll"],
    )
}

/// Two dropdowns, one of them left alone. The record carries both, so a run
/// that stored the right plan on the wrong billing period fails with the
/// reason visible.
fn choose_from_a_dropdown() -> Case {
    nav_case(
        "nav-select-plan",
        goal("nav-select.html", "store the Business plan billed yearly."),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            records("plan.plan", json!("Business")),
            records("plan.period", json!("Yearly")),
        ],
        &[REVIEW_TAG, "browser", "select"],
    )
}

/// A goal that can only be reached with a keystroke: the page has no button,
/// and typing into the field does nothing. `PRESS_KEY` is offered only for
/// the focused control (`jev-nav/js/snapshot.js`), which the field's
/// `autofocus` provides.
fn press_a_key() -> Case {
    nav_case(
        "nav-press-key",
        goal(
            "nav-keys.html",
            "arm the shortcut by pressing the down arrow in the shortcut field.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            records("shortcut.armed", json!(true)),
            records("shortcut.key", json!("ArrowDown")),
        ],
        &[REVIEW_TAG, "browser", "keys"],
    )
}

/// Typing is not choosing. The field owns a listbox, and only picking the
/// suggestion records a destination — the case the navigator's own rules call
/// out ("a typed query still needs its matching autocomplete suggestion
/// selected").
fn choose_an_autocomplete_suggestion() -> Case {
    nav_case(
        "nav-autocomplete",
        goal(
            "nav-autocomplete.html",
            "choose Lisbon, Portugal as the destination.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            records("destination", json!("Lisbon, Portugal")),
        ],
        &[REVIEW_TAG, "browser", "autocomplete"],
    )
}

/// Two steps, and the second one proves the first: opening a result records
/// the query that produced it, so a run that clicked a result without
/// searching is visible in the record rather than hidden by a plausible
/// answer.
fn search_then_open_a_result() -> Case {
    nav_case(
        "nav-search-open-result",
        goal(
            "nav-search.html",
            "search for suppliers in Bilbao and open Casa Metal.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            records("opened.name", json!("Casa Metal")),
            records_containing("opened.query", "Bilbao"),
        ],
        &[REVIEW_TAG, "browser", "multi-step"],
    )
}

/// The no-progress tripwire, on the page that used to defeat it.
///
/// Nothing here can mark a shipment delivered, and the only thing that ever
/// changes is a badge that moves — no text, no control state, no URL. A
/// fingerprint carrying geometry reported progress forever on exactly this
/// shape (16 §1.3), so the case asserts two things: the page was not changed,
/// and the navigator stopped instead of claiming the goal was met.
fn refuse_to_call_an_animation_progress() -> Case {
    nav_case(
        "nav-animation-no-progress",
        goal("nav-animation.html", "mark shipment 4471 delivered."),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            navigator_stopped("Blocked after"),
            records_nothing(),
        ],
        &[REVIEW_TAG, "browser", "rules"],
    )
}

/// The confirm gate, both halves: the card is published for the label rule,
/// and approving it *completes the work*.
///
/// Before the card path existed this run simply ended, so the second half
/// could not be asserted at all — which is the whole point of 16 §5.3. The
/// button records a payment and sends nothing anywhere.
fn pay_only_with_an_approval() -> Case {
    nav_case(
        "nav-confirm-pay",
        goal("nav-pay.html", "pay invoice INV-2291."),
        Some(Cards::approving()),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            // The deterministic label rule, not a safety head's opinion.
            confirm_card("label:pay", "confirmed"),
            records("paid.invoice", json!("INV-2291")),
        ],
        &[REVIEW_TAG, "browser", "confirm"],
    )
}

/// A login wall, which is a pause and not a failure (B2).
///
/// The page shows a password field, so the rules layer hands over before any
/// classifier is asked; the harness plays the person who signs in — it opens
/// the fixture's session from a tab of its own, exactly as a human would in
/// the headed profile — and answers the card. The run then resumes against a
/// page that genuinely changed and finishes the job, which is what makes the
/// resume real rather than a re-try.
fn resume_after_a_login_wall() -> Case {
    nav_case(
        "nav-login-wall",
        goal("nav-login.html", "mark invoice INV-4417 seen."),
        Some(Cards::signing_in(pages::state_url())),
        vec![
            // A hand-over is not an error: the run is alive, waiting.
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            ask_card("Sign in and I'll pick up where I stopped."),
            records("seen.invoice", json!("INV-4417")),
        ],
        &[REVIEW_TAG, "browser", "needs-user"],
    )
}

/// A destination the run may never follow.
///
/// The page's only way onward is a `mailto:` link, and the deterministic
/// layer refuses any non-`http(s)` destination outright — a refusal, not a
/// card: there is nothing a person could approve here.
///
/// This is also as close as the suite can get to the denied-origin rule from
/// the agent path: `run_browser` passes `denied_origins: Vec::new()` because
/// settings carry no list yet (`neo-agent/src/agent/tools.rs`), so a
/// denied-host case would assert against an empty rule. Both refusals are the
/// same code path in `Gate::destination`.
fn refuse_a_non_web_destination() -> Case {
    nav_case(
        "nav-nonweb-link",
        goal(
            "nav-nonweb.html",
            "reach the delivery desk about shipment 4471.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            navigator_stopped("is not a web page"),
            no_cards(),
            records_nothing(),
        ],
        &[REVIEW_TAG, "browser", "rules"],
    )
}

/// Reading a fact off a page and answering with it — the shape of most GTM
/// research, and the one case whose deliverable *is* the answer.
///
/// Scored against the page's own published values rather than a literal in
/// this file: the fixture records what its table says, so the assertion
/// follows the page if the page changes, and a model answering from memory
/// cannot pass — the reference is not a string that exists anywhere else.
fn answer_a_fact_off_the_page() -> Case {
    nav_case(
        "nav-extract-fact",
        goal(
            "nav-fact.html",
            "tell me the reconciliation reference and the closing balance.",
        ),
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            Assertion::Custom(Box::new(|output| {
                let record = recorded(output)?;
                let reference = dig(&record, "ledger.reference")
                    .and_then(Value::as_str)
                    .ok_or("the fixture never published its reference")?;
                let balance = dig(&record, "ledger.balance")
                    .and_then(Value::as_str)
                    .ok_or("the fixture never published its balance")?;
                let answer = output.final_text.replace(',', "");
                let missing: Vec<&str> = [reference, balance]
                    .into_iter()
                    .filter(|wanted| !answer.contains(&wanted.replace(',', "")))
                    .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "the answer is missing what the page says: {missing:?} — it said \
                         {:.200}",
                        output.final_text
                    ))
                }
            })),
        ],
        &[REVIEW_TAG, "browser", "extract"],
    )
}

// ------------------------------------------- the three real-site cases (live)

/// A live case: a real site, an accessibility read of Chrome's own window for
/// the probe (there is no fixture record to read), and the navigator's own
/// report of where it ended.
fn live_case(
    id: &str,
    message: &str,
    cards: Option<Cards>,
    assertions: Vec<Assertion>,
    tags: &[&str],
) -> Case {
    build(
        App::Chrome,
        id,
        message,
        Probe::AppText {
            app: App::Chrome.selector().to_owned(),
        },
        None,
        cards,
        assertions,
        tags,
    )
}

/// The multi-step shape against a real search box, because a fixture's search
/// cannot reproduce a real site's redirects, consent banners and lazy loads.
fn search_and_open_on_a_real_site() -> Case {
    live_case(
        "nav-live-search-open",
        "Open https://en.wikipedia.org/wiki/Main_Page, search for Ada Lovelace \
         and open her article.",
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            ended_at("/wiki/Ada_Lovelace"),
            window_shows("Ada Lovelace"),
        ],
        &[LIVE_TAG, "browser", "live", "multi-step"],
    )
}

/// Reading a fact off a page nobody here controls.
fn read_a_fact_off_a_real_site() -> Case {
    live_case(
        "nav-live-read-fact",
        "Open https://www.rfc-editor.org/rfc/rfc9110.html and tell me the \
         title of that RFC.",
        None,
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            Assertion::ExpectTextContains("HTTP Semantics".to_owned()),
            window_shows("9110"),
        ],
        &[LIVE_TAG, "browser", "live", "extract"],
    )
}

/// Bar B4: the default browser run completes a goal behind a login the user
/// performed once, in the profile their logins live in (16 §0).
///
/// The only case in the suite that depends on a human having done something
/// beforehand, which is why it is live-tagged and named in RELEASING.md. On a
/// profile that was never signed in, the run hands over, the harness answers
/// "I'm ready" — it cannot sign in to somebody else's site — and the case
/// fails on the window still showing a sign-in page. That failure is the
/// honest reading of B4 not holding.
fn work_behind_a_login_the_user_performed() -> Case {
    live_case(
        "nav-live-signed-in",
        "Open https://github.com/notifications and tell me the subject of the \
         most recent notification, or that there are none.",
        Some(Cards {
            confirm: crate::cards::Confirm::Deny,
            ask: Some(crate::cards::Ask::Ready),
        }),
        vec![
            Assertion::ExpectNoError,
            Assertion::ExpectTools(vec!["browse".to_owned()]),
            ended_at("github.com/notifications"),
            Assertion::Custom(Box::new(|output| {
                let probe = output
                    .tool_calls_by_name("probe")
                    .first()
                    .map(|call| call.arguments.clone())
                    .ok_or("no probe ran")?;
                let window = probe
                    .get("window")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_lowercase();
                if window.contains("sign in") {
                    Err(format!(
                        "the profile is not signed in — Chrome's window is {window:?}; \
                         sign in once in the managed Chrome (B4)"
                    ))
                } else {
                    Ok(())
                }
            })),
        ],
        &[LIVE_TAG, "browser", "live", "b4"],
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

    /// The cases that are expected to fail today are tagged, so a report can
    /// tell a regression from a known gap.
    #[test]
    fn the_known_gaps_are_tagged() {
        let gaps: Vec<String> = all()
            .into_iter()
            .filter(|case| case.test.tags.iter().any(|tag| tag == "known-gap"))
            .map(|case| case.test.id)
            .collect();
        assert!(gaps.contains(&"write-a-calc-cell".to_owned()));
        assert!(gaps.contains(&"spreadsheet-offers-menus".to_owned()));
        // The agent's browse tool carries no attachments, so the upload half
        // of the review set cannot pass yet (see `attach_a_file`).
        assert!(gaps.contains(&"nav-upload-confirm".to_owned()));
    }

    /// Bar B7 is "a **fifteen**-task navigation review set", and the tag is
    /// what a release runs. A sixteenth case under the same tag silently
    /// changes the bar, so the count is the assertion.
    #[test]
    fn the_review_set_is_fifteen_tasks_under_one_tag() {
        let tagged: Vec<String> = all()
            .into_iter()
            .filter(|case| case.test.tags.iter().any(|tag| tag == REVIEW_TAG))
            .map(|case| case.test.id)
            .collect();
        assert_eq!(
            tagged.len(),
            15,
            "the review set must be exactly fifteen tasks, got {tagged:?}"
        );
        assert_eq!(review_set().len(), 15);

        // Every shape the plan names has a case, addressed by id so a rename
        // cannot quietly drop one.
        for wanted in [
            "nav-form-submit",
            "nav-cross-origin-frame",
            "nav-shadow-field",
            "nav-upload-confirm",
            "nav-popup-adopt",
            "nav-nested-scroll",
            "nav-select-plan",
            "nav-press-key",
            "nav-autocomplete",
            "nav-search-open-result",
            "nav-animation-no-progress",
            "nav-confirm-pay",
            "nav-login-wall",
            "nav-nonweb-link",
            "nav-extract-fact",
        ] {
            assert!(tagged.contains(&wanted.to_owned()), "{wanted} is missing");
        }
    }

    /// The gate has to be runnable without the network. A review-set case
    /// that named a real site would make the release gate depend on somebody
    /// else's uptime — which is exactly why the three live cases carry their
    /// own tag instead of this one.
    #[test]
    fn the_review_set_only_drives_local_fixtures() {
        for case in all() {
            let review = case.test.tags.iter().any(|tag| tag == REVIEW_TAG);
            let live = case.test.tags.iter().any(|tag| tag == LIVE_TAG);
            assert!(!(review && live), "{} is in both sets", case.test.id);
            if review {
                assert!(
                    case.test.user_message.contains(&pages::url("")),
                    "{} must drive the fixture server, not {}",
                    case.test.id,
                    case.test.user_message
                );
                assert!(
                    matches!(
                        Probe::from_config(&case.test.config),
                        Some(Probe::Page { .. })
                    ),
                    "{} must be scored on the page's own record",
                    case.test.id
                );
                assert!(
                    matches!(
                        Fixture::from_config(&case.test.config),
                        Some(Fixture::WebPage { .. })
                    ),
                    "{} must clear the record before it runs, or it scores the \
                     previous case's leftovers",
                    case.test.id
                );
            }
        }
    }

    /// At most three cases may need a real site (16 §6.2), and each one has to
    /// say so in its tags: a release runs the gate without them.
    #[test]
    fn the_live_cases_are_few_and_labelled() {
        let live: Vec<String> = all()
            .into_iter()
            .filter(|case| case.test.tags.iter().any(|tag| tag == LIVE_TAG))
            .inspect(|case| {
                assert!(
                    case.test.tags.iter().any(|tag| tag == "live"),
                    "{} must be excludable by the `live` tag too",
                    case.test.id
                );
            })
            .map(|case| case.test.id)
            .collect();
        assert!(live.len() <= 3, "too many real-site cases: {live:?}");
        assert_eq!(live.len(), live_review_set().len());
    }

    /// A card the harness answers is a deliberate part of a case, and
    /// approving is never the default: a case that trips an unexpected confirm
    /// must refuse it, so the report says which rule fired instead of
    /// laundering it.
    #[test]
    fn only_the_cases_that_mean_to_approve_do() {
        let approving: Vec<String> = all()
            .into_iter()
            .filter(|case| {
                Cards::from_config(&case.test.config)
                    .is_some_and(|cards| cards.confirm == crate::cards::Confirm::Approve)
            })
            .map(|case| case.test.id)
            .collect();
        assert_eq!(approving, ["nav-upload-confirm", "nav-confirm-pay"]);
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
