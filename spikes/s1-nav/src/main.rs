//! Spike S1: launch a managed Chrome, observe a page, and (with keys) run the Jev navigator.
//!
//!   s1-nav <url> "<goal>" [--headed] [--profile DIR] [--attach FILE] [--no-safety]
//!
//! Keys come from `.env` beside the workspace: TYPESAFE_API_KEY, and for typing
//! OPENAI_API_KEY (+ optional TEXT_MODEL, default gpt-5.6-luna). With no TypeSafe key
//! the spike still measures launch + snapshot and prints the element table.

use std::time::Instant;

use jev_nav::policy::action_space;
use jev_nav::text::OpenAiTextHelper;
use jev_nav::web::CdpObserver;
use jev_nav::wire::TypeSafe;
use jev_nav::{Navigator, RunConfig};
use neo_cdp::{Browser, LaunchOptions};
use serde_json::{Value, json};

fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };
    for line in text.lines() {
        if let Some((name, value)) = line.split_once('=') {
            let name = name.trim();
            if !name.is_empty() && !name.starts_with('#') && std::env::var(name).is_err() {
                // Single-threaded at this point: nothing else reads the environment yet.
                unsafe { std::env::set_var(name, value.trim().trim_matches('"')) };
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let headed = args.iter().any(|a| a == "--headed");
    let safety = !args.iter().any(|a| a == "--no-safety");
    let mut profile_path = None;
    let mut attachments = Vec::new();
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--profile" => {
                index += 1;
                profile_path = args.get(index).map(std::path::PathBuf::from);
            }
            "--attach" => {
                index += 1;
                if let Some(path) = args.get(index) {
                    attachments.push(std::path::PathBuf::from(path));
                }
            }
            value if !value.starts_with("--") => positional.push(value),
            _ => {}
        }
        index += 1;
    }
    let url = positional
        .first()
        .copied()
        .unwrap_or("https://en.wikipedia.org/");
    let goal = positional.get(1).copied().unwrap_or("");

    let temporary_profile = if profile_path.is_none() {
        Some(tempfile::tempdir()?)
    } else {
        None
    };
    let profile_path = profile_path.unwrap_or_else(|| {
        temporary_profile
            .as_ref()
            .expect("temporary profile")
            .path()
            .to_owned()
    });
    let mut options = LaunchOptions::new(profile_path);
    options.headless = !headed;

    let timer = Instant::now();
    let browser = Browser::launch(&options).await?;
    println!(
        "chrome launch + connect   {:>6} ms",
        timer.elapsed().as_millis()
    );

    let timer = Instant::now();
    let page = browser.new_page("about:blank").await?;
    page.set_viewport(1120, 780, 1.0).await?;
    page.navigate(url).await?;
    println!(
        "new tab + navigate        {:>6} ms   {url}",
        timer.elapsed().as_millis()
    );

    let mut observer = CdpObserver::new(page).with_attachments(attachments);
    let mut samples = Vec::new();
    let mut observation = Value::Null;
    for _ in 0..5 {
        let timer = Instant::now();
        observation = observer.observe().await?;
        samples.push(timer.elapsed().as_micros() as f64 / 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let actions: Vec<_> = observation["actions"]
        .as_array()
        .map(|l| l.iter().filter_map(|a| a.as_object().cloned()).collect())
        .unwrap_or_default();
    let space = action_space(&actions);
    println!(
        "snapshot (median of 5)    {:>6.1} ms   {} actions, {} elements, {} chars of text, {} omitted",
        samples[2],
        actions.len(),
        space.elements.len(),
        observation["text"].as_str().map(str::len).unwrap_or(0),
        observation["omitted_actions"]
    );
    for element in space.elements.iter().take(12) {
        println!(
            "  [{}] {:<10} {}",
            element["index"].as_str().unwrap_or("?"),
            element["role"].as_str().unwrap_or(""),
            element["label"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(70)
                .collect::<String>()
        );
    }

    let Ok(typesafe_key) = std::env::var("TYPESAFE_API_KEY") else {
        println!("\nTYPESAFE_API_KEY not set — stopping after the observation half of the spike.");
        browser.close().await;
        return Ok(());
    };
    if goal.is_empty() {
        println!("\nno goal given — stopping after observation.");
        browser.close().await;
        return Ok(());
    }

    let text: Option<Box<dyn jev_nav::text::TextHelper>> = std::env::var("OPENAI_API_KEY").ok().map(|key| {
        let model = std::env::var("TEXT_MODEL").unwrap_or_else(|_| "gpt-5.6-luna".into());
        let base_url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".into());
        let mut helper = OpenAiTextHelper::new(key, model, base_url);
        helper.extra = json!({ "reasoning_effort": std::env::var("TEXT_REASONING").unwrap_or_else(|_| "none".into()) });
        Box::new(helper) as Box<dyn jev_nav::text::TextHelper>
    });
    if text.is_none() {
        println!("OPENAI_API_KEY not set — the run will stop at the first TYPE_TEXT.");
    }

    let typesafe_endpoint = std::env::var("TYPESAFE_ENDPOINT")
        .unwrap_or_else(|_| "https://api.typesafe.ai/v1/systemone".into());
    let typesafe_model = std::env::var("TYPESAFE_MODEL").unwrap_or_else(|_| "jev-latest".into());
    println!("\ngoal: {goal}\n");
    let mut navigator = Navigator::new(
        observer,
        TypeSafe::new(typesafe_key, typesafe_endpoint, typesafe_model),
        text,
    );
    let config = RunConfig {
        goal: goal.to_owned(),
        safety_heads: safety,
        confirm_at: 0.4,
        // A spike has nobody to answer a card, so it runs with no denied
        // list and simply reports whatever ending it reaches.
        denied_origins: Vec::new(),
    };
    let mut jev = Vec::new();
    let started = Instant::now();
    let calls_before = browser.calls();
    let outcome = navigator
        .run(&config, |step| {
            jev.push(step.jev_ms);
            let safety: Vec<String> = step.decision.safety.iter().map(|(k, v)| format!("{k}={v:.2}")).collect();
            println!(
                "{:>6} ms  {:<11} {:<44} p={:.2} tgt={}  obs {:>3} · jev {:>4} · text {:>4} · act {:>3} ms  [{} cands]{}{}  {}",
                step.elapsed.as_millis(),
                step.decision.operation,
                step.label.clone().unwrap_or_default().chars().take(44).collect::<String>(),
                step.decision.operation_confidence,
                step.decision.target_confidence.map(|c| format!("{c:.2}")).unwrap_or_else(|| "-".into()),
                step.observe_ms,
                step.jev_ms,
                step.text_ms,
                step.act_ms,
                step.candidates,
                step.typed.as_ref().map(|t| format!("  typed {t:?}")).unwrap_or_default(),
                if step.stale { "  STALE" } else { "" },
                safety.join(" "),
            );
        })
        .await;
    let total = started.elapsed();
    jev.sort();
    println!("\noutcome: {outcome:?}");
    println!(
        "total {} ms · {} jev requests (median {} ms) · {} actions · {} protocol calls",
        total.as_millis(),
        jev.len(),
        jev.get(jev.len() / 2).copied().unwrap_or(0),
        navigator.history().len(),
        browser.calls() - calls_before
    );
    let end = navigator
        .observer
        .page()
        .evaluate("[location.href, document.title]")
        .await?;
    println!("ended at: {end}");
    browser.close().await;
    Ok(())
}
