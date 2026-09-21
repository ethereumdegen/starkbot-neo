#![forbid(unsafe_code)]

mod eval;
mod fetch_codex;
mod nav;

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use neo_agent::ax::AxRequest;
use neo_agent::codex::{
    CodexSupervisor, CodexSupervisorConfig, configured_executable, default_codex_home,
};
use neo_agent::runtime::Runtime;
use neo_core::ProviderAccount;
use neo_store::{ProviderAccountRepository, Store};
use tokio::process::Command;
use zeroize::Zeroize;

#[derive(Parser)]
#[command(name = "neo", version, about = "Starkbot Neo headless interface")]
struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true, value_name = "PATH")]
    codex_bin: Option<PathBuf>,
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Subscription connections (K6 paths b and d). `--provider` chooses which
    /// one; the default is the Claude subscription, the path that works today.
    Account {
        #[arg(long, default_value = neo_core::PROVIDER_CLAUDE_SUBSCRIPTION)]
        provider: String,
        #[command(subcommand)]
        command: AccountCommand,
    },
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
    /// Direct-key accounts: the credentials Starkbot itself owns (05 §6).
    Keys {
        #[command(subcommand)]
        command: KeyCommand,
    },
    /// The model registry: cached catalogues, refreshes and resolution.
    Models {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// One turn on the runtime `settings.models.inference` selects.
    Ask {
        #[arg(long)]
        model: Option<String>,
        /// Ask for JSON matching this schema instead of prose.
        #[arg(long, value_name = "JSON")]
        schema: Option<String>,
        prompt: String,
    },
    /// Read settings, or merge-patch one section (04 §14 `patch_settings`).
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    /// Drive a web page to a goal with Jev (10, spike S1 promoted).
    Nav {
        url: String,
        goal: String,
        /// Run without a window, in a profile that dies with the run. The
        /// default is the app's own Chrome, headed, where logins persist.
        #[arg(long)]
        headless: bool,
        /// Ask no safety heads. Fixtures only: nothing can trip the gate.
        #[arg(long)]
        no_safety: bool,
        /// A file a file input may be given. Repeatable.
        #[arg(long, value_name = "PATH")]
        attach: Vec<PathBuf>,
        /// Use this Chrome profile directory instead of the app's own.
        #[arg(long, value_name = "PATH")]
        profile: Option<PathBuf>,
    },
    /// Is this machine ready to run a task? (05 §10). Exits non-zero when
    /// something must be fixed first.
    Doctor,
    /// Read and drive a native app directly (01 §CLI surface).
    Ax {
        #[command(subcommand)]
        command: AxCommand,
    },
    /// Drive a native macOS app to a goal with Jev (01, A23).
    App {
        /// Bundle id, pid, or a name substring: `TextEdit`, `LibreOffice`.
        app: String,
        goal: String,
    },
    /// Named standing work and its per-project heartbeat.
    Projects {
        #[command(subcommand)]
        command: Option<ProjectCommand>,
    },
    /// Run project heartbeats.
    Heartbeat {
        #[command(subcommand)]
        command: HeartbeatCommand,
    },
    /// Run the terminal front end (P12, plans/14-tui.md).
    Tui,
    /// Run the desktop front end (P12, `src-tauri`).
    Gui {
        /// Build the app instead of running it, leaving a binary behind.
        #[arg(long)]
        build: bool,
    },
    /// Every Starkbot running on this machine, and what each is doing.
    Sessions,
    /// Run the app-control evaluation suite (agent in the loop, real apps).
    Eval {
        /// Only cases whose id or name contains this.
        #[arg(long)]
        filter: Option<String>,
        /// Only cases with any of these tags (`browser`, `app`, `spreadsheet`,
        /// `media`, `smoke`, `known-gap`, `nav-review`, `nav-review-live`).
        #[arg(long)]
        tag: Vec<String>,
        /// Run each case once instead of the five-run consensus, for a quick
        /// look. Never use this to claim a case passes.
        #[arg(long)]
        once: bool,
        /// Write the JSON report here, for `--baseline` on a later run.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Compare against a report written earlier and print the diff.
        #[arg(long)]
        baseline: Option<PathBuf>,
        /// List the cases and which apps are installed, without running.
        #[arg(long)]
        list: bool,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    Status,
    Login,
    Logout,
    Models,
}

/// The subset of 01 §CLI surface that exists today: enough to see what Jev
/// sees, and to press one thing by hand.
#[derive(Subcommand)]
enum AxCommand {
    /// Is this binary trusted for Accessibility?
    Trusted,
    /// Every running app with a pid.
    Apps,
    /// The element table Jev would be shown.
    Table {
        /// Bundle id, pid, or name substring.
        app: String,
    },
    /// Press one row of the current table.
    Press { app: String, index: u16 },
    /// Put text into one row of the current table.
    Set {
        app: String,
        index: u16,
        text: String,
    },
    /// Type into whatever is focused, through CGEvent.
    Type { app: String, text: String },
    /// Press one key in whatever is focused: return, escape, tab, space,
    /// delete, up, down, left, right.
    Key { app: String, key: String },
    /// Invoke a menu path: `neo ax menu TextEdit "Format › Make Plain Text"`.
    Menu {
        app: String,
        /// Levels separated by `›` or `>`.
        path: String,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Create a managed project, or register an existing directory.
    Add {
        name: String,
        #[arg(long, value_name = "PATH")]
        root: Option<PathBuf>,
    },
    /// Show the two documents, clock, and recent ticks.
    Show { project: String },
    /// Open one project document in $EDITOR.
    Edit {
        project: String,
        #[arg(long, conflicts_with = "soul", required_unless_present = "soul")]
        heartbeat: bool,
        #[arg(
            long,
            conflicts_with = "heartbeat",
            required_unless_present = "heartbeat"
        )]
        soul: bool,
    },
    /// Configure this project's clock.
    Heartbeat {
        project: String,
        #[arg(long, value_name = "DURATION")]
        every: Option<String>,
        #[arg(long, conflicts_with = "off")]
        on: bool,
        #[arg(long, conflicts_with = "on")]
        off: bool,
    },
}

#[derive(Subcommand)]
enum HeartbeatCommand {
    /// Run one project's heartbeat now, in the foreground.
    Run { project: String },
}

#[derive(Subcommand)]
enum SettingsCommand {
    /// Print the whole validated settings object.
    Get,
    /// Merge-patch one section: `neo settings patch models '{"inference":…}'`.
    Patch { section: String, patch: String },
    /// Point inference (and, unless told otherwise, the text helper) at one of
    /// the four K6 runtimes.
    UseRuntime {
        provider: String,
        #[arg(default_value = neo_core::SOL_LATEST)]
        id: String,
    },
}

/// A key is never passed as an argument (05 §6), so no variant here takes one.
#[derive(Subcommand)]
enum KeyCommand {
    /// Report every core account and the state of its credential.
    Status,
    /// Read a key from stdin, store it, and validate it.
    Set { account: String },
    /// Forget a key.
    Rm { account: String },
    /// Re-validate one account, or every account.
    Check { account: Option<String> },
    /// Re-seal every stored credential to the binary running now, so macOS
    /// stops asking for authorisation on every read.
    Reseal,
    /// Copy credentials out of the login Keychain into the dev key file, so a
    /// `cargo` build never prompts again. Asks once per item.
    ImportLogin,
}

/// The model registry (05 §7). A refresh needs that runtime's key; the cache
/// serves whatever the last refresh left.
#[derive(Subcommand)]
enum ModelCommand {
    /// Print one runtime's cached catalogue.
    List {
        #[arg(default_value = "openai")]
        provider: String,
    },
    /// Re-read one runtime's catalogue from the vendor and replace the cache.
    Refresh {
        #[arg(default_value = "openai")]
        account: String,
    },
    /// Show what a saved id resolves to against the cached catalogue,
    /// including the symbolic `sol-latest`.
    Resolve {
        #[arg(default_value = "openai")]
        provider: String,
        #[arg(default_value = neo_core::SOL_LATEST)]
        id: String,
    },
}

#[derive(Subcommand)]
enum DevCommand {
    FetchCodex {
        #[arg(long)]
        target: Option<String>,
        #[arg(long, value_name = "PATH")]
        manifest: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        destination: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // The surface is how a report groups a trace, and a `neo tui` session is
    // a different thing to read than a one-shot command — so the subcommand
    // is peeked at here, before `init`, which happens once per process and
    // cannot be told again later.
    let tui = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with('-'))
        .is_some_and(|argument| argument == "tui");
    neo_otel::init(if tui { "neo-tui" } else { "neo-cli" });
    let Cli {
        data_dir,
        codex_bin,
        command,
    } = Cli::parse();
    let result = run(data_dir, codex_bin, command).await;
    // A short `neo ask` finishes well inside the exporter's flush interval,
    // so without this its spans would leave with the process. An endpoint
    // that is missing or unreachable costs a bounded wait and never the
    // command's result.
    neo_otel::shutdown().await;
    result
}

/// The command itself, split from [`main`] so every exit path flushes the
/// trace on its way out rather than each one remembering to.
async fn run(
    data_dir: Option<PathBuf>,
    codex_bin: Option<PathBuf>,
    command: CommandKind,
) -> Result<()> {
    match command {
        CommandKind::Dev {
            command:
                DevCommand::FetchCodex {
                    target,
                    manifest,
                    destination,
                },
        } => {
            let target = target.unwrap_or(fetch_codex::host_target()?.into());
            let manifest = manifest.unwrap_or_else(fetch_codex::default_manifest);
            let destination =
                destination.unwrap_or_else(|| fetch_codex::default_destination(&target));
            fetch_codex::fetch(&manifest, &target, &destination).await?;
            print_json(&serde_json::json!({
                "target": target,
                "path": destination,
                "verified": true
            }))?;
            Ok(())
        }
        CommandKind::Account { provider, command } => {
            run_account(data_dir, codex_bin, provider, command).await
        }
        CommandKind::Keys { command } => run_keys(data_dir, command).await,
        CommandKind::Ask {
            model,
            schema,
            prompt,
        } => run_ask(data_dir, model, schema, prompt).await,
        CommandKind::Settings { command } => run_settings(data_dir, command).await,
        CommandKind::Nav {
            url,
            goal,
            headless,
            no_safety,
            attach,
            profile,
        } => {
            let runtime = std::sync::Arc::new(open_runtime(data_dir)?);
            nav::run(
                runtime,
                nav::NavOptions {
                    url,
                    goal,
                    headless,
                    no_safety,
                    attach,
                    profile,
                },
            )
            .await
        }
        CommandKind::Ax { command } => {
            let runtime = std::sync::Arc::new(open_runtime(data_dir)?);
            nav::run_ax(runtime, command_of(command)).await
        }
        CommandKind::App { app, goal } => {
            let runtime = std::sync::Arc::new(open_runtime(data_dir)?);
            nav::run_app(runtime, nav::AppNavOptions { app, goal }).await
        }
        CommandKind::Projects { command } => run_projects(data_dir, command).await,
        CommandKind::Heartbeat { command } => run_heartbeat(data_dir, command).await,
        CommandKind::Doctor => run_doctor(data_dir),
        CommandKind::Sessions => run_sessions(data_dir),
        CommandKind::Tui => run_tui(data_dir).await,
        CommandKind::Gui { build } => run_gui(build).await,
        CommandKind::Eval {
            filter,
            tag,
            once,
            report,
            baseline,
            list,
        } => {
            eval::run(
                data_dir,
                eval::EvalOptions {
                    filter,
                    tags: tag,
                    once,
                    report,
                    baseline,
                    list,
                },
            )
            .await
        }
        CommandKind::Models { command } => run_models(data_dir, command).await,
    }
}

/// `neo settings`: the same `patch_settings` both front ends call, so a
/// headless run can choose its inference runtime before any UI exists.
async fn run_settings(data_dir: Option<PathBuf>, command: SettingsCommand) -> Result<()> {
    let runtime = open_runtime(data_dir)?;
    match command {
        SettingsCommand::Get => {
            let settings = tokio::task::block_in_place(|| runtime.settings())?;
            print_json(&settings)
        }
        SettingsCommand::Patch { section, patch } => {
            let patch: serde_json::Value =
                serde_json::from_str(&patch).context("the patch must be a JSON value")?;
            let settings = tokio::task::block_in_place(|| runtime.patch_settings(&section, patch))?;
            print_json(&settings)
        }
        SettingsCommand::UseRuntime { provider, id } => {
            let patch = serde_json::json!({
                "inference": { "provider": provider, "id": id }
            });
            let settings = tokio::task::block_in_place(|| runtime.patch_settings("models", patch))?;
            print_json(&settings.models)
        }
    }
}

/// The CLI's `AxCommand` as the library's own request type, so the clap
/// types stay in this file and never reach `neo-agent`.
fn command_of(command: AxCommand) -> AxRequest {
    match command {
        AxCommand::Trusted => AxRequest::Trusted,
        AxCommand::Apps => AxRequest::Apps,
        AxCommand::Table { app } => AxRequest::Table { app },
        AxCommand::Press { app, index } => AxRequest::Press { app, index },
        AxCommand::Set { app, index, text } => AxRequest::Set { app, index, text },
        AxCommand::Type { app, text } => AxRequest::Type { app, text },
        AxCommand::Key { app, key } => AxRequest::Key { app, key },
        AxCommand::Menu { app, path } => AxRequest::Menu { app, path },
    }
}

/// `neo doctor`: every local readiness check, and a non-zero exit when a task
/// cannot run (05 §10).
fn run_doctor(data_dir: Option<PathBuf>) -> Result<()> {
    let runtime = open_runtime(data_dir)?;
    let report = tokio::task::block_in_place(|| runtime.doctor())?;
    print_json(&report)?;
    if report.ready() {
        Ok(())
    } else {
        Err(anyhow!(
            "not ready: {} check(s) must be fixed first",
            report
                .checks
                .iter()
                .filter(|check| check.health == neo_agent::doctor::Health::Fail)
                .count()
        ))
    }
}

/// Every command that only needs the façade opens it the same way.
fn open_runtime(data_dir: Option<PathBuf>) -> Result<Runtime> {
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    tokio::task::block_in_place(|| Runtime::open(&data_dir))
        .context("could not open Starkbot Neo's runtime")
}

/// `neo models` (05 §15 day 4): the cache, a refresh, and what a saved id
/// means today. Never needs a subscription helper.
async fn run_models(data_dir: Option<PathBuf>, command: ModelCommand) -> Result<()> {
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    let runtime = tokio::task::block_in_place(|| Runtime::open(&data_dir))
        .context("could not open Starkbot Neo's runtime")?;
    match command {
        ModelCommand::List { provider } => {
            let models = tokio::task::block_in_place(|| runtime.models(&provider))?;
            print_json(&models_json(&models))
        }
        ModelCommand::Refresh { account } => {
            let models = runtime.refresh_models(&account).await?;
            print_json(&models_json(&models))
        }
        ModelCommand::Resolve { provider, id } => {
            let models = tokio::task::block_in_place(|| runtime.models(&provider))?;
            let ids: Vec<String> = models
                .iter()
                .map(|model| model.info.reference.id.clone())
                .collect();
            print_json(&serde_json::json!({
                "provider": provider,
                "requested": id,
                "resolved": neo_core::resolve(&provider, &id, &ids),
                "catalog_size": ids.len(),
            }))
        }
    }
}

/// The cache as JSON: the model plus when this runtime first and last offered
/// it, which is what makes a disappearing id explainable.
fn models_json(models: &[neo_store::CachedModel]) -> serde_json::Value {
    serde_json::Value::Array(
        models
            .iter()
            .map(|model| {
                serde_json::json!({
                    "id": model.info.reference.id,
                    "provider": model.info.reference.provider.as_str(),
                    "use_cases": model.info.use_cases,
                    "capabilities": model.info.capabilities,
                    "price": model.info.price,
                    "hidden": model.hidden,
                    "first_seen": model.first_seen,
                    "last_seen": model.last_seen,
                })
            })
            .collect(),
    )
}

/// The TUI is a synchronous, terminal-owning loop over the synchronous
/// `Runtime`, so it runs on a blocking-capable thread. It deliberately does not
/// resolve the Codex helper: the terminal front end boots with no inference
/// connection at all (14 §7, M1).
/// `neo sessions` — the machine-local roster.
fn run_sessions(data_dir: Option<PathBuf>) -> Result<()> {
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    let runtime = tokio::task::block_in_place(|| Runtime::open(&data_dir))
        .context("could not open Starkbot Neo's runtime")?;
    let mine = runtime.announced_session();
    let sessions = tokio::task::block_in_place(|| runtime.sessions())?;
    print_json(&serde_json::Value::Array(
        sessions
            .into_iter()
            .map(|session| {
                serde_json::json!({
                    "kind": session.kind.as_str(),
                    "pid": session.pid,
                    "host": session.host,
                    "activity": session.activity,
                    "started_at": session.started_at,
                    "last_seen": session.last_seen,
                    "is_me": Some(session.id) == mine,
                })
            })
            .collect(),
    ))
}

async fn run_tui(data_dir: Option<PathBuf>) -> Result<()> {
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("could not create {}", data_dir.display()))?;
    // The front end runs a subscription login as a background task on this
    // runtime, so it needs a shared handle rather than a borrow.
    let runtime = std::sync::Arc::new(
        tokio::task::block_in_place(|| Runtime::open(&data_dir))
            .context("could not open Starkbot Neo's runtime")?,
    );
    tokio::task::block_in_place(|| {
        neo_tui::run(runtime).context("the terminal front end stopped with an error")
    })
}

/// `neo gui`: start the desktop front end.
///
/// The desktop is a second binary with a web front end, and in a development
/// build its window loads `devUrl` — so starting `target/debug/neo-desktop`
/// by hand shows an empty window unless a Vite server is already up on 1420.
/// `tauri dev` is what starts both halves in the right order, and reaching for
/// it here is the difference between one command and three.
///
/// This is the one place the CLI shells out to the build tool, which only
/// makes sense from a checkout; a shipped Starkbot has an app bundle and does
/// not need it.
async fn run_gui(build: bool) -> Result<()> {
    let root = workspace_root()?;
    let action = if build { "build" } else { "dev" };
    eprintln!("neo gui: cargo tauri {action} in {}", root.display());
    let mut command = Command::new("cargo");
    command.arg("tauri").arg(action).current_dir(&root);
    if let Some((variable, value)) = webkit_dmabuf_workaround() {
        eprintln!(
            "neo gui: {variable}={value} — WebKit's DMABUF renderer trips this \
             compositor's explicit-sync check"
        );
        command.env(variable, value);
    }
    let status = command.status().await.context(
        "could not start `cargo tauri`. Install the Tauri CLI with `cargo install tauri-cli`",
    )?;
    if !status.success() {
        return Err(anyhow!(
            "`cargo tauri {action}` exited with {}",
            status
                .code()
                .map_or_else(|| "a signal".to_owned(), |code| code.to_string())
        ));
    }
    Ok(())
}

/// The one environment variable a Linux desktop start may need set for it.
///
/// WebKitGTK's DMABUF renderer and the NVIDIA driver disagree about explicit
/// sync: the web process takes a `wp_linux_drm_syncobj_surface_v1` and then
/// commits a buffer with no acquire point, so the compositor drops the
/// connection with `wl_display.error(…, "Missing acquire timeline")`. What
/// the user sees is a window that appears and dies a second later, reported
/// as `Gdk-Message: Error 71 (Protocol error)` — and every start does it,
/// because Hyprland and KWin both advertise the protocol. Turning the
/// renderer off falls back to shared-memory buffers: slower, and present.
///
/// Narrow on purpose. Mesa drives the same path correctly, so the variable is
/// set only where the fault is, and an explicit value from the user always
/// wins over this.
fn webkit_dmabuf_workaround() -> Option<(&'static str, &'static str)> {
    const VARIABLE: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    if !cfg!(target_os = "linux") || std::env::var_os(VARIABLE).is_some() {
        return None;
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let nvidia = Path::new("/sys/module/nvidia_drm").exists();
    (wayland && nvidia).then_some((VARIABLE, "1"))
}

/// The checkout holding `src-tauri`.
///
/// Searched upwards from the working directory first, so a `neo` on `PATH`
/// drives whichever checkout the user is standing in; the directory this
/// binary was compiled from is the fallback, which is what makes `neo gui`
/// work from anywhere.
fn workspace_root() -> Result<PathBuf> {
    let marker = Path::new("src-tauri").join("tauri.conf.json");
    let cwd = std::env::current_dir().context("the working directory is not readable")?;
    for directory in cwd.ancestors() {
        if directory.join(&marker).is_file() {
            return Ok(directory.to_owned());
        }
    }
    let compiled_from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|directory| directory.join(&marker).is_file())
        .map(Path::to_owned);
    compiled_from.ok_or_else(|| {
        anyhow!(
            "no Starkbot checkout found: `{}` is not under {} or any parent of the working \
             directory. Run `neo gui` from the repository, or open the installed app.",
            marker.display(),
            cwd.display()
        )
    })
}

/// The direct-key commands of 05 §15 day 3. Everything here goes through
/// `Runtime` — the CLI never touches the Keychain or a validator itself — and
/// none of it needs the Codex helper: keys and subscriptions are separate
/// paths (05 §6, K6).
async fn run_keys(data_dir: Option<PathBuf>, command: KeyCommand) -> Result<()> {
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    let runtime = tokio::task::block_in_place(|| Runtime::open(&data_dir))
        .context("could not open Starkbot Neo's runtime")?;
    match command {
        KeyCommand::Status => {
            let status = tokio::task::block_in_place(|| runtime.key_status())?;
            print_json(&status)
        }
        KeyCommand::Set { account } => {
            let account = known_account(&runtime, &account)?;
            let mut raw = read_secret(&account)?;
            // Store first, then validate, so the 04 §14 contract holds even if
            // the machine is offline: the key is kept and re-checked later.
            let stored = tokio::task::block_in_place(|| runtime.set_key(&account, &raw));
            raw.zeroize();
            stored?;
            print_json(&runtime.check_key(&account).await?)
        }
        KeyCommand::ImportLogin => {
            let imported = tokio::task::block_in_place(|| runtime.import_login_keychain())?;
            print_json(&serde_json::json!({
                "imported": imported,
                "into": std::env::var("NEO_KEYCHAIN_FILE").unwrap_or_else(|_| "login keychain (nothing to do)".into()),
            }))
        }
        KeyCommand::Reseal => {
            let resealed = tokio::task::block_in_place(|| runtime.reseal_credentials())?;
            print_json(&serde_json::json!({
                "resealed": resealed,
                "note": "each item asked once; later reads are silent while this binary keeps its signature",
            }))
        }
        KeyCommand::Rm { account } => {
            let account = known_account(&runtime, &account)?;
            let status = tokio::task::block_in_place(|| runtime.remove_key(&account))?;
            print_json(&status)
        }
        KeyCommand::Check {
            account: Some(account),
        } => {
            let account = known_account(&runtime, &account)?;
            print_json(&runtime.check_key(&account).await?)
        }
        KeyCommand::Check { account: None } => {
            let mut checked = Vec::new();
            for account in accounts(&runtime)? {
                checked.push(runtime.check_key(&account).await?);
            }
            print_json(&checked)
        }
    }
}

/// The accounts the core owns, in the core's order. Reading them off the
/// runtime keeps `neo-keys` out of the CLI's dependency list (05 §1).
fn accounts(runtime: &Runtime) -> Result<Vec<String>> {
    Ok(tokio::task::block_in_place(|| runtime.key_status())?
        .into_iter()
        .map(|status| status.account)
        .collect())
}

fn known_account(runtime: &Runtime, account: &str) -> Result<String> {
    let known = accounts(runtime)?;
    if known.iter().any(|candidate| candidate == account) {
        return Ok(account.to_owned());
    }
    Err(anyhow!(
        "unknown key account: {account}; Starkbot owns {}. ChatGPT and the Claude subscription are not key accounts — connect those with `neo account login`",
        known.join(", ")
    ))
}

/// A key never reaches argv and is never echoed (05 §6). Piped input is read
/// whole; a terminal gets a one-line prompt on stderr and reads with echo off.
fn read_secret(account: &str) -> Result<String> {
    let raw = if std::io::stdin().is_terminal() {
        eprintln!("Paste the {account} key, then press Enter (nothing is echoed):");
        read_secret_without_echo()?
    } else {
        let mut piped = String::new();
        std::io::stdin()
            .read_to_string(&mut piped)
            .context("could not read the key from stdin")?;
        let trimmed = piped.trim().to_owned();
        piped.zeroize();
        trimmed
    };
    if raw.is_empty() {
        return Err(anyhow!(
            "no key on stdin; pipe one in (`… | neo keys set {account}`) or paste it at the prompt"
        ));
    }
    Ok(raw)
}

/// Raw mode is the echo switch: the terminal stops printing what is typed, so
/// this loop reassembles the line itself. Raw mode is always given back.
fn read_secret_without_echo() -> Result<String> {
    crossterm::terminal::enable_raw_mode().context("could not turn terminal echo off")?;
    let typed = read_typed_secret();
    let restored = crossterm::terminal::disable_raw_mode();
    eprintln!();
    restored.context("could not turn terminal echo back on")?;
    typed
}

fn read_typed_secret() -> Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

    let mut buffer = String::new();
    loop {
        let Event::Key(key) = crossterm::event::read().context("could not read the key")? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return Ok(buffer),
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char('c' | 'd') if control => {
                buffer.zeroize();
                return Err(anyhow!("cancelled; nothing was stored"));
            }
            KeyCode::Char(character) if !control => buffer.push(character),
            _ => {}
        }
    }
}

/// One turn on the selected inference runtime (K6). Which runtime that is
/// comes from settings, so `neo settings use-runtime …` is how you switch;
/// `--model` and `--schema` are per-call.
async fn run_ask(
    data_dir: Option<PathBuf>,
    model: Option<String>,
    schema: Option<String>,
    prompt: String,
) -> Result<()> {
    let runtime = open_runtime(data_dir)?;
    match schema {
        Some(schema) => {
            let schema: serde_json::Value =
                serde_json::from_str(&schema).context("--schema must be a JSON schema")?;
            let (value, turn) = runtime.ask_json(&prompt, &schema, model.as_deref()).await?;
            print_json(&serde_json::json!({
                "json": value,
                "model": turn.model,
                "duration_ms": turn.duration_ms,
                "usage": turn.usage,
            }))
        }
        None => {
            let turn = runtime.ask(&prompt, model.as_deref()).await?;
            print_json(&serde_json::json!({
                "text": turn.text,
                "model": turn.model,
                "duration_ms": turn.duration_ms,
                "usage": turn.usage,
            }))
        }
    }
}

async fn run_projects(data_dir: Option<PathBuf>, command: Option<ProjectCommand>) -> Result<()> {
    let runtime = std::sync::Arc::new(open_runtime(data_dir)?);
    match command {
        None => print_json(&runtime.projects()?),
        Some(ProjectCommand::Add { name, root }) => {
            print_json(&runtime.create_project(&name, root.as_deref())?)
        }
        Some(ProjectCommand::Show { project }) => {
            let row = runtime.project(&project)?;
            let documents = runtime.project_documents(&project)?;
            let ticks = runtime.project_ticks(&project, 20)?;
            print_json(&serde_json::json!({
                "project": row,
                "soul": documents.soul,
                "heartbeat": documents.heartbeat,
                "ticks": ticks,
            }))
        }
        Some(ProjectCommand::Edit {
            project,
            heartbeat,
            soul: _,
        }) => {
            let row = runtime.project(&project)?;
            let document = if heartbeat { "heartbeat.md" } else { "soul.md" };
            let path = Path::new(&row.root).join(document);
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_owned());
            let status = std::process::Command::new(&editor)
                .arg(&path)
                .status()
                .with_context(|| format!("could not start editor `{editor}`"))?;
            if !status.success() {
                return Err(anyhow!("editor `{editor}` exited unsuccessfully"));
            }
            Ok(())
        }
        Some(ProjectCommand::Heartbeat {
            project,
            every,
            on,
            off,
        }) => {
            let current = runtime.project(&project)?;
            let seconds = every
                .as_deref()
                .map(parse_duration)
                .transpose()?
                .unwrap_or(current.heartbeat_every_seconds);
            let enabled = if on {
                true
            } else if off {
                false
            } else {
                current.heartbeat_enabled
            };
            print_json(&runtime.configure_project_heartbeat(
                &project,
                enabled,
                seconds,
                current.on_gate,
            )?)
        }
    }
}

async fn run_heartbeat(data_dir: Option<PathBuf>, command: HeartbeatCommand) -> Result<()> {
    let runtime = std::sync::Arc::new(open_runtime(data_dir)?);
    match command {
        HeartbeatCommand::Run { project } => {
            print_json(&runtime.run_project_heartbeat(&project).await?)
        }
    }
}

fn parse_duration(value: &str) -> Result<u64> {
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b'm') => (&value[..value.len() - 1], 60),
        Some(b'h') => (&value[..value.len() - 1], 60 * 60),
        Some(b'd') => (&value[..value.len() - 1], 24 * 60 * 60),
        _ => (value, 1),
    };
    let amount = number
        .parse::<u64>()
        .with_context(|| format!("invalid duration `{value}`"))?;
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("duration `{value}` is too large"))
}

/// `neo account --provider …`: the two subscription paths (K6 b and d). The
/// Claude path drives the vendor's own CLI through `Runtime`; the ChatGPT path
/// drives the pinned Codex app-server.
async fn run_account(
    data_dir: Option<PathBuf>,
    codex_bin: Option<PathBuf>,
    provider: String,
    command: AccountCommand,
) -> Result<()> {
    match provider.as_str() {
        neo_core::PROVIDER_CLAUDE_SUBSCRIPTION => run_claude_account(data_dir, command).await,
        neo_core::PROVIDER_CHATGPT_CODEX => run_codex_account(data_dir, codex_bin, command).await,
        id if id == neo_agent::oauth::ANTHROPIC_OAUTH.id => {
            run_oauth_account(data_dir, &neo_agent::oauth::ANTHROPIC_OAUTH, command).await
        }
        id if id == neo_agent::oauth::OPENAI_CODEX.id => {
            run_oauth_account(data_dir, &neo_agent::oauth::OPENAI_CODEX, command).await
        }
        other => Err(anyhow!(
            "`{other}` is not a subscription path; use `{}`, `{}`, `{}` or `{}`",
            neo_agent::oauth::ANTHROPIC_OAUTH.id,
            neo_agent::oauth::OPENAI_CODEX.id,
            neo_core::PROVIDER_CLAUDE_SUBSCRIPTION,
            neo_core::PROVIDER_CHATGPT_CODEX
        )),
    }
}

/// The two subscription paths whose OAuth tokens Starkbot owns (K7).
///
/// `login` opens the vendor's own consent page in the user's browser and waits
/// on the loopback callback; nothing but the resulting credential is stored,
/// and it goes to the Keychain.
async fn run_oauth_account(
    data_dir: Option<PathBuf>,
    provider: &'static neo_agent::oauth::OauthProvider,
    command: AccountCommand,
) -> Result<()> {
    let runtime = open_runtime(data_dir)?;
    match command {
        AccountCommand::Status => print_json(&runtime.oauth_account(provider).await?),
        AccountCommand::Login => {
            // The prompt goes to stderr so `neo account … login` still pipes
            // clean JSON on stdout.
            eprintln!(
                "Opening {} in your browser; finish the sign-in there.",
                provider.authorize_url
            );
            print_json(&runtime.connect_oauth(provider).await?)
        }
        AccountCommand::Logout => print_json(&runtime.disconnect_oauth(provider)?),
        AccountCommand::Models => {
            let models = tokio::task::block_in_place(|| runtime.models(provider.id))?;
            print_json(&models_json(&models))
        }
    }
}

/// The Claude subscription: `claude auth …` in Starkbot's own config home,
/// with only the redacted row persisted (K6, 05 §6).
async fn run_claude_account(data_dir: Option<PathBuf>, command: AccountCommand) -> Result<()> {
    let runtime = open_runtime(data_dir)?;
    let account = match command {
        AccountCommand::Status => runtime.refresh_claude_account().await?,
        AccountCommand::Login => runtime.connect_claude().await?,
        AccountCommand::Logout => runtime.disconnect_claude().await?,
        AccountCommand::Models => {
            return Err(anyhow!(
                "the Claude subscription exposes no model catalogue to list; \
                 `neo settings use-runtime claude-subscription <model>` selects one"
            ));
        }
    };
    print_json(&account)
}

async fn run_codex_account(
    data_dir: Option<PathBuf>,
    codex_bin: Option<PathBuf>,
    command: AccountCommand,
) -> Result<()> {
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    let store = open_store(&data_dir).await?;
    let executable = resolve_codex_executable(codex_bin)?;
    let mut config = CodexSupervisorConfig::new(executable, default_codex_home(&data_dir));
    config.request_timeout = Duration::from_secs(20);
    let supervisor = CodexSupervisor::launch(config)
        .await
        .context("could not start the pinned Codex app-server")?;

    match command {
        AccountCommand::Status => {
            let account = sync_account(supervisor.client(), store.provider_accounts()).await?;
            print_json(&account)?;
        }
        AccountCommand::Login => {
            let attempt = supervisor.client().start_chatgpt_login().await?;
            let login_id = attempt.start().login_id.clone();
            open_browser(attempt.start().auth_url.as_str()).await?;
            if let Err(error) = attempt.wait(Duration::from_secs(300)).await {
                let _ = supervisor.client().cancel_login(&login_id).await;
                return Err(error.into());
            }
            let account = sync_account(supervisor.client(), store.provider_accounts()).await?;
            print_json(&account)?;
        }
        AccountCommand::Logout => {
            supervisor.client().logout().await?;
            let account = sync_account(supervisor.client(), store.provider_accounts()).await?;
            print_json(&account)?;
        }
        AccountCommand::Models => {
            let models = supervisor.client().list_models().await?;
            print_json(&models)?;
        }
    }

    supervisor.shutdown().await?;
    Ok(())
}

async fn open_store(data_dir: &Path) -> Result<Store> {
    let database = data_dir.join("neo.db");
    let backups = data_dir.join("backups");
    tokio::task::spawn_blocking(move || Store::open(database, backups))
        .await
        .context("store initialization task failed")?
        .context("could not open Starkbot Neo's store")
}

async fn sync_account(
    client: &neo_agent::codex::CodexClient,
    repository: ProviderAccountRepository,
) -> Result<ProviderAccount> {
    let account = client.read_provider_account().await?;
    let saved = account.clone();
    tokio::task::spawn_blocking(move || repository.put(saved))
        .await
        .context("account persistence task failed")?
        .context("could not persist redacted account state")?;
    Ok(account)
}

fn default_data_dir() -> Result<PathBuf> {
    neo_core::paths::data_dir().ok_or_else(|| anyhow!("HOME is not set"))
}

fn resolve_codex_executable(cli_value: Option<PathBuf>) -> Result<PathBuf> {
    let path = cli_value
        .or_else(|| configured_executable().map(PathBuf::from))
        .or_else(|| {
            fetch_codex::host_target()
                .ok()
                .map(fetch_codex::default_destination)
        })
        .ok_or_else(|| {
            anyhow!(
                "Codex helper is unavailable; run `neo dev fetch-codex`, pass --codex-bin, or set NEO_CODEX_BIN in the development CLI"
            )
        })?;
    if !path.is_file() {
        return Err(anyhow!(
            "Codex helper does not exist at {}; run `neo dev fetch-codex`",
            path.display()
        ));
    }
    Ok(path)
}

async fn open_browser(url: &str) -> Result<()> {
    let status = Command::new(neo_agent::runtime::URL_OPENER)
        .arg(url)
        .status()
        .await
        .context("could not open the ChatGPT login URL")?;
    if !status.success() {
        return Err(anyhow!("the system browser rejected the ChatGPT login URL"));
    }
    Ok(())
}

#[allow(clippy::print_stdout)]
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let encoded = serde_json::to_string_pretty(value)?;
    println!("{encoded}");
    Ok(())
}
