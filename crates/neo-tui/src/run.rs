//! Terminal lifecycle and the event loop.
//!
//! This is the only module that touches the terminal, the clock or the core.
//! Everything it decides is decided by `state.rs` and `keys.rs`.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use neo_agent::agent::{
    AppOptions, BrowserOptions, ChatMessage, ChatRequest, run_app, run_browser,
};
use neo_agent::ax::{AxRequest, AxResponse};
use neo_agent::oauth::{ANTHROPIC_OAUTH, OPENAI_CODEX, OauthProvider};
use neo_agent::runtime::{LoginHandle, Runtime, RuntimeError};
use neo_core::{Envelope, RunId};
use neo_eval::Selection;
use neo_voice::{Microphone, Transcriber};
use ratatui::DefaultTerminal;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::keys::{Action, KeyMap};
use crate::runs::RunKind;
use crate::state::{Command, Login, LoginPhase, NavSpec, SessionRow, State, key_label, plan_title};
use crate::ui;

/// How often this process refreshes its roster row and its leases. A third of
/// the lease TTL, so two beats may be missed before anything expires.
const HEARTBEAT: Duration = Duration::from_secs(10);

/// The coalescing tick from 14 §4: a burst of events produces one frame.
const TICK: Duration = Duration::from_millis(60);
/// A second kill switch inside this window quits (14 §3).
const DOUBLE_KILL: Duration = Duration::from_secs(2);
/// How long quitting waits for the cancelled runs to let go of what they
/// hold. Long enough for a CDP `Browser.close` round trip, short enough
/// that a wedged run cannot hold the terminal hostage.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(5);
/// How long the core serves the loopback callback: a browser round-trip with a
/// password manager and a second factor in the middle.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// How much of a thread is read back on open or on a switch.
const THREAD_LIMIT: u32 = 200;
/// How many conversations the switcher lists.
const SESSION_LIMIT: u32 = 50;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("the terminal could not be driven")]
    Terminal(#[from] io::Error),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// Take over the terminal, run the front end, and give the terminal back on
/// every exit path — clean return, error, or panic.
pub fn run(runtime: Arc<Runtime>) -> Result<(), TuiError> {
    let mut guard = TerminalGuard::new()?;
    event_loop(&runtime, &mut guard.terminal)
}

/// The microphone, opened on first use and kept, plus the transcriber.
///
/// Both are built lazily: opening the input device turns on the OS microphone
/// indicator, and building the transcriber reads the Keychain, so neither may
/// happen on a launch where the user never dictates.
struct Dictation {
    microphone: Microphone,
    transcriber: Arc<dyn Transcriber>,
}

/// This process's row on the machine-local roster.
///
/// A front end that did not announce itself is invisible to the others and
/// holds no leases, so a second Starkbot would drive the same applications
/// underneath it.
struct Presence {
    runtime: Arc<Runtime>,
    id: Option<String>,
    last_beat: Instant,
}

impl Presence {
    fn join(runtime: &Arc<Runtime>) -> Self {
        let id = match runtime.announce(neo_store::SessionKind::Tui) {
            Ok(id) => Some(id),
            // A roster that cannot be written must not stop the front end.
            Err(error) => {
                tracing::warn!(%error, "could not announce this session");
                None
            }
        };
        Self {
            runtime: Arc::clone(runtime),
            id,
            last_beat: Instant::now(),
        }
    }

    /// Report what this process is doing, at most once per [`HEARTBEAT`].
    fn beat(&mut self, activity: &str) {
        let Some(id) = self.id.as_deref() else {
            return;
        };
        if self.last_beat.elapsed() < HEARTBEAT {
            return;
        }
        self.last_beat = Instant::now();
        if let Err(error) = self.runtime.heartbeat(id, Some(activity)) {
            tracing::warn!(%error, "could not refresh this session");
        }
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        if let Some(id) = self.id.as_deref()
            && let Err(error) = self.runtime.depart(id)
        {
            tracing::warn!(%error, "could not leave the roster");
        }
    }
}

/// What a finished transcription sends back to the frame loop.
enum VoiceUpdate {
    Text(String),
    Failed(String),
}

/// How one run ended, as the call itself returned it.
///
/// This is *not* how progress arrives — progress is [`neo_core::AppEvent`],
/// reduced by `state.rs`, which is what lets any number of observers follow
/// a run. It is only the terminal result, which the event stream does not
/// carry for a `:nav`, a `:ax` or a suite: those report their outcome as a
/// return value, and a run that failed before publishing anything would
/// otherwise sit at `running` forever.
struct Settled {
    run: RunId,
    /// Lines to append to the trace before the run settles.
    detail: Vec<String>,
    outcome: Result<String, String>,
}

/// One run in flight, so `x` and the kill switch can reach its token.
struct Job {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Drop for Job {
    /// Cancelling is what reclaims the browser and stops the typing: the
    /// token reaches the navigator and the AX actor, whereas `abort` alone
    /// drops the future and leaves Chrome running.
    ///
    /// So this cancels, and only cancels. It used to abort in the next
    /// breath, which set the flag before the cancelled task could be polled
    /// even once — producing precisely the orphan the paragraph above says
    /// the cancel is for, on the one path that reaches here: quitting.
    /// [`Jobs::settle`] is what gives those tasks their moment to run.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Every run this front end has started and not yet reaped.
struct Jobs {
    active: HashMap<RunId, Job>,
    settled: mpsc::Receiver<Settled>,
    sender: mpsc::Sender<Settled>,
}

impl Jobs {
    fn new() -> Self {
        // One slot per concurrent run is plenty: each job sends exactly once.
        let (sender, settled) = mpsc::channel(32);
        Self {
            active: HashMap::new(),
            settled,
            sender,
        }
    }

    /// Spawn `work` as a run, keeping its token so it can be cancelled.
    ///
    /// `work` is handed the token rather than being aborted, because an
    /// aborted future drops mid-step and leaves whatever it launched running.
    fn spawn<F, Fut>(&mut self, run: RunId, work: F) -> CancellationToken
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(Vec<String>, String), String>> + Send,
    {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let sender = self.sender.clone();
        let task = tokio::spawn(async move {
            let settled = match work(token).await {
                Ok((detail, summary)) => Settled {
                    run,
                    detail,
                    outcome: Ok(summary),
                },
                Err(error) => Settled {
                    run,
                    detail: Vec::new(),
                    outcome: Err(error),
                },
            };
            let _ = sender.send(settled).await;
        });
        self.active.insert(
            run,
            Job {
                cancel: cancel.clone(),
                task,
            },
        );
        cancel
    }

    fn cancel(&self, run: RunId) {
        if let Some(job) = self.active.get(&run) {
            job.cancel.cancel();
        }
    }

    fn cancel_all(&self) {
        for job in self.active.values() {
            job.cancel.cancel();
        }
    }

    /// Cancel everything and wait, briefly, for it to let go.
    ///
    /// A cancelled task still has to be *polled* to close its browser and
    /// release the keyboard; returning from the loop drops the runtime with
    /// those futures still parked at their cancellation points, so the quit
    /// path asks for the shutdown and then stays long enough to observe it.
    /// Bounded, because "reclaim the browser" must not become "the terminal
    /// never comes back": whatever has not finished by the deadline is left
    /// to the runtime, which is no worse than the abort this replaced.
    fn settle(&mut self, budget: Duration) {
        self.cancel_all();
        if self.active.is_empty() {
            return;
        }
        let active = &mut self.active;
        let closing = async {
            for job in active.values_mut() {
                // The task's own error is not interesting here: a run that
                // panicked has already dropped whatever it was holding.
                let _ = (&mut job.task).await;
            }
        };
        let closed =
            tokio::runtime::Handle::current().block_on(tokio::time::timeout(budget, closing));
        if closed.is_err() {
            tracing::warn!(
                ?budget,
                "a run did not stop in time; leaving it to the runtime"
            );
        }
    }
}

/// A subscription login in flight (K7).
///
/// The handle is shared, not moved: the loopback wait borrows it, and so does
/// a pasted redirect URL, so the paste fallback stays available for the whole
/// login — including after the loopback wait has timed out.
struct LoginWorker {
    provider: &'static OauthProvider,
    handle: Arc<LoginHandle>,
    outcome: mpsc::Receiver<Result<neo_core::ProviderAccount, RuntimeError>>,
    sender: mpsc::Sender<Result<neo_core::ProviderAccount, RuntimeError>>,
    tasks: Vec<JoinHandle<()>>,
    deadline: Instant,
}

impl LoginWorker {
    /// Begin the login, start serving the callback, and open the browser.
    fn start(
        runtime: &Arc<Runtime>,
        provider: &'static OauthProvider,
    ) -> Result<(Self, String), RuntimeError> {
        let handle = Arc::new(runtime.begin_oauth(provider)?);
        let url = handle.authorize_url().to_string();
        let (sender, outcome) = mpsc::channel(4);
        let mut worker = Self {
            provider,
            handle,
            outcome,
            sender,
            tasks: Vec::new(),
            deadline: Instant::now() + LOGIN_TIMEOUT,
        };
        // Bind and serve before the browser can redirect back.
        worker.spawn_wait(runtime);
        worker.spawn_open(runtime, &url);
        Ok((worker, url))
    }

    fn spawn_wait(&mut self, runtime: &Arc<Runtime>) {
        let runtime = Arc::clone(runtime);
        let handle = Arc::clone(&self.handle);
        let sender = self.sender.clone();
        self.tasks.push(tokio::spawn(async move {
            let result = runtime.finish_oauth(&handle, LOGIN_TIMEOUT).await;
            // A closed channel means the user cancelled; that is not an error.
            let _ = sender.send(result).await;
        }));
    }

    fn spawn_open(&mut self, runtime: &Arc<Runtime>, url: &str) {
        let runtime = Arc::clone(runtime);
        let url = url.to_owned();
        self.tasks.push(tokio::spawn(async move {
            if let Err(error) = runtime.open_in_browser(&url) {
                tracing::warn!(%error, "could not open the vendor page");
            }
        }));
    }

    /// Exchange a redirect URL the user pasted. Races the loopback wait; the
    /// first code to be exchanged wins and the other result is discarded.
    fn spawn_paste(&mut self, runtime: &Arc<Runtime>, pasted: String) {
        let runtime = Arc::clone(runtime);
        let handle = Arc::clone(&self.handle);
        let sender = self.sender.clone();
        self.tasks.push(tokio::spawn(async move {
            let result = runtime.finish_oauth_pasted(&handle, &pasted).await;
            let _ = sender.send(result).await;
        }));
    }

    fn seconds_left(&self) -> u64 {
        self.deadline
            .saturating_duration_since(Instant::now())
            .as_secs()
    }
}

impl Drop for LoginWorker {
    /// Cancelling a login must release the loopback port, not leave a listener
    /// holding 54545 until the process exits.
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Restores the terminal on drop. `ratatui::try_init` additionally installs a
/// panic hook that restores before the message is printed, so a panic cannot
/// leave a half-drawn alternate screen (14 §5).
struct TerminalGuard {
    terminal: DefaultTerminal,
}

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        Ok(Self {
            terminal: ratatui::try_init()?,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(error) = ratatui::try_restore() {
            tracing::warn!(%error, "could not restore the terminal");
        }
    }
}

/// Everything the loop owns that is not `State`.
struct Loop {
    jobs: Jobs,
    login: Option<LoginWorker>,
    dictation: Option<Dictation>,
    voice: Option<mpsc::Receiver<VoiceUpdate>>,
    /// The loop's own monotonic origin, so the reducer can be handed a clock
    /// without reading one itself.
    started: Instant,
}

fn event_loop(runtime: &Arc<Runtime>, terminal: &mut DefaultTerminal) -> Result<(), TuiError> {
    // Join the machine-local roster, so another Starkbot can see this one and
    // so this one's app runs can take the keyboard lease. Dropping the guard
    // on any exit path leaves the roster and releases the leases.
    let mut session = Presence::join(runtime);
    let mut receiver = runtime.subscribe();
    let mut state = State::new(runtime.bootstrap()?);
    // The thread is loaded from the store, so closing the TUI does not throw
    // the conversation away.
    match runtime.open_conversation() {
        Ok(conversation) => {
            let messages = runtime.thread(conversation.id, THREAD_LIMIT)?;
            state.load_thread(conversation.id, conversation.title, &messages);
        }
        Err(error) => state.note(format!("the conversation could not be opened: {error}")),
    }
    let mut context = Loop {
        jobs: Jobs::new(),
        login: None,
        dictation: None,
        voice: None,
        started: Instant::now(),
    };

    let outcome = frames(
        runtime,
        terminal,
        &mut state,
        &mut context,
        &mut receiver,
        &mut session,
    );
    // Quitting is the path that used to orphan a browser: the loop returned,
    // the jobs were dropped, and their cancellation never got polled. Every
    // exit comes through here, including an error one, because a run holding
    // Chrome does not care why the front end is leaving.
    context.jobs.settle(SHUTDOWN_BUDGET);
    outcome
}

/// Draw, read a key, apply what the core published; repeat until quit.
fn frames(
    runtime: &Arc<Runtime>,
    terminal: &mut DefaultTerminal,
    state: &mut State,
    context: &mut Loop,
    receiver: &mut tokio::sync::broadcast::Receiver<Envelope>,
    session: &mut Presence,
) -> Result<(), TuiError> {
    let keymap = KeyMap;
    let mut last_kill: Option<Instant> = None;
    // The sequence number the next envelope should carry. `None` until the
    // first one arrives, since a subscription starts wherever the process is.
    let mut expected: Option<u64> = None;

    loop {
        if state.dirty {
            terminal.draw(|frame| ui::draw(frame, state))?;
            state.dirty = false;
        }

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => {
                    state.status = None;
                    let action = keymap.resolve(key, state);
                    if action == Action::KillSwitch {
                        if last_kill.is_some_and(|at| at.elapsed() < DOUBLE_KILL) {
                            return Ok(());
                        }
                        last_kill = Some(Instant::now());
                    }
                    if let Some(command) = state.apply_action(action) {
                        execute(runtime, terminal, state, context, command)?;
                    }
                }
                Event::Resize(..) => state.dirty = true,
                _ => {}
            }
        }

        state.tick(elapsed_ms(context.started));
        poll_login(runtime, state, &mut context.login)?;
        poll_jobs(state, &mut context.jobs);
        poll_voice(state, &context.dictation, &mut context.voice);
        drain(runtime, state, receiver, &mut expected)?;
        // What this process is doing, in the words another Starkbot will see.
        session.beat(state.activity_line());

        if state.quit {
            return Ok(());
        }
    }
}

/// The loop's monotonic clock, in milliseconds since it started.
fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Open the microphone and start recording.
///
/// Failures are reported as a status line naming the fix — a denied
/// permission is a thing the user can act on, not an error to swallow.
fn start_dictation(runtime: &Arc<Runtime>, state: &mut State, dictation: &mut Option<Dictation>) {
    if dictation.is_none() {
        let transcriber = match runtime.transcriber() {
            Ok(transcriber) => Arc::from(transcriber),
            Err(error) => {
                state.note(format!("dictation: {error}"));
                return;
            }
        };
        match Microphone::open(None) {
            Ok(microphone) => {
                *dictation = Some(Dictation {
                    microphone,
                    transcriber,
                });
            }
            Err(error) => {
                state.note(format!("dictation: {error}"));
                return;
            }
        }
    }
    let Some(active) = dictation.as_mut() else {
        return;
    };
    match active.microphone.start() {
        Ok(()) => {
            state.set_dictating(true);
            state.note(format!(
                "listening — {} again to stop · {}",
                if state.mode == crate::state::Mode::Insert {
                    "Ctrl-V"
                } else {
                    "v"
                },
                active.transcriber.name()
            ));
        }
        Err(error) => state.note(format!("dictation: {error}")),
    }
}

/// Stop recording and transcribe off the frame loop.
fn stop_dictation(
    state: &mut State,
    dictation: &mut Option<Dictation>,
    voice: &mut Option<mpsc::Receiver<VoiceUpdate>>,
) {
    let Some(active) = dictation.as_mut() else {
        return;
    };
    state.set_dictating(false);
    let utterance = match active.microphone.stop() {
        Ok(utterance) => utterance,
        Err(error) => {
            state.note(format!("dictation: {error}"));
            return;
        }
    };
    state.note(format!(
        "transcribing {:.1}s…",
        utterance.duration.as_secs_f32()
    ));
    let transcriber = Arc::clone(&active.transcriber);
    let (sender, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        let update = match transcriber.transcribe(&utterance).await {
            Ok(transcript) => VoiceUpdate::Text(transcript.text),
            Err(error) => VoiceUpdate::Failed(error.to_string()),
        };
        let _ = sender.send(update).await;
    });
    *voice = Some(receiver);
}

/// Pick up a finished transcription, and keep the level meter moving.
fn poll_voice(
    state: &mut State,
    dictation: &Option<Dictation>,
    voice: &mut Option<mpsc::Receiver<VoiceUpdate>>,
) {
    if state.dictating
        && let Some(active) = dictation.as_ref()
    {
        state.set_level(active.microphone.level());
    }
    let Some(receiver) = voice.as_mut() else {
        return;
    };
    match receiver.try_recv() {
        Ok(VoiceUpdate::Text(text)) => {
            state.dictated(&text);
            *voice = None;
        }
        Ok(VoiceUpdate::Failed(detail)) => {
            state.note(format!("dictation: {detail}"));
            *voice = None;
        }
        Err(mpsc::error::TryRecvError::Empty) => {}
        Err(mpsc::error::TryRecvError::Disconnected) => *voice = None,
    }
}

/// Reap whatever finished since the last frame.
fn poll_jobs(state: &mut State, jobs: &mut Jobs) {
    loop {
        match jobs.settled.try_recv() {
            Ok(settled) => {
                jobs.active.remove(&settled.run);
                state.finish_run(settled.run, settled.detail, settled.outcome);
            }
            Err(mpsc::error::TryRecvError::Empty) => return,
            // A closed channel is not an idle one. Treating the two alike
            // meant a task that died without sending left its run "running"
            // for ever, with nothing on screen to say why — which is exactly
            // how a hung job presents. Every live run is settled as failed so
            // the user gets an answer and the pane stops lying.
            Err(mpsc::error::TryRecvError::Disconnected) => {
                let live: Vec<RunId> = jobs.active.keys().copied().collect();
                for run in live {
                    jobs.active.remove(&run);
                    state.finish_run(
                        run,
                        Vec::new(),
                        Err("the run stopped without reporting".to_owned()),
                    );
                }
                return;
            }
        }
    }
}

/// Move a login forward without blocking the frame: tick the countdown, and
/// pick up the result the moment a background task produces one.
fn poll_login(
    runtime: &Arc<Runtime>,
    state: &mut State,
    login: &mut Option<LoginWorker>,
) -> Result<(), TuiError> {
    let Some(worker) = login.as_mut() else {
        return Ok(());
    };
    match worker.outcome.try_recv() {
        Ok(Ok(account)) => {
            if let Some(overlay) = state.login.as_mut() {
                overlay.phase = LoginPhase::Done;
            }
            state.note(format!(
                "{} is connected{}",
                plan_title(worker.provider.id),
                account
                    .email
                    .as_deref()
                    .map(|email| format!(" as {email}"))
                    .unwrap_or_default()
            ));
            *login = None;
            state.rebootstrap(runtime.bootstrap()?);
        }
        Ok(Err(error)) => {
            // Keep the overlay up: the message names what to do next, and the
            // paste fallback is still usable.
            if let Some(overlay) = state.login.as_mut() {
                overlay.phase = LoginPhase::Failed(error.to_string());
            }
            state.dirty = true;
        }
        Err(mpsc::error::TryRecvError::Empty) => {
            let left = worker.seconds_left();
            if let Some(overlay) = state.login.as_mut()
                && overlay.remaining != left
            {
                overlay.remaining = left;
                state.dirty = true;
            }
        }
        Err(mpsc::error::TryRecvError::Disconnected) => {
            if let Some(overlay) = state.login.as_mut() {
                overlay.phase = LoginPhase::Failed("the login task stopped".into());
            }
            *login = None;
            state.dirty = true;
        }
    }
    Ok(())
}

/// Apply everything the core has published since the last frame.
///
/// A break in [`Envelope::seq`] means this front end is not looking at the
/// stream it thinks it is — a thread rendered from a partial one is worse
/// than one rendered again from scratch, so a break re-bootstraps (14 §4).
/// `seq` is what detects it, not `Lagged`: a burst that arrives while this
/// loop is inside `terminal.draw` is missed without the channel ever
/// reporting it.
///
/// A number *below* what was expected counts. It means the publisher
/// restarted its counter or a prefix is being replayed, and the previous
/// `seq > next` test accepted that silently — applying a second copy of
/// every event it had already seen.
fn drain(
    runtime: &Arc<Runtime>,
    state: &mut State,
    receiver: &mut tokio::sync::broadcast::Receiver<Envelope>,
    expected: &mut Option<u64>,
) -> Result<(), TuiError> {
    loop {
        match receiver.try_recv() {
            Ok(Envelope { seq, event, .. }) => {
                let broken = expected.is_some_and(|next| seq != next);
                *expected = Some(seq + 1);
                if broken {
                    rebootstrap(runtime, state)?;
                    state.note("the event stream broke — re-bootstrapped");
                }
                state.apply(event);
            }
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Closed) => {
                state.note("the core stopped publishing events");
                return Ok(());
            }
            Err(TryRecvError::Lagged(dropped)) => {
                // The next envelope's `seq` reports the gap too; this only
                // keeps the counter from blaming the following event.
                *expected = None;
                rebootstrap(runtime, state)?;
                state.note(format!("{dropped} events dropped — re-bootstrapped"));
            }
        }
    }
}

/// Re-read everything the core owns, including the thread: a gap may have
/// swallowed an `AppEvent::Message`, and a thread missing one line is worse
/// than one read again.
fn rebootstrap(runtime: &Arc<Runtime>, state: &mut State) -> Result<(), TuiError> {
    state.rebootstrap(runtime.bootstrap()?);
    if let Some(conversation) = state.conversation {
        let title = state.conversation_title.clone();
        let messages = runtime.thread(conversation, THREAD_LIMIT)?;
        state.load_thread(conversation, title, &messages);
    }
    Ok(())
}

/// One agent turn, and one message steered into a turn already running.
///
/// The turn itself is not recorded here any more: the core persists the
/// answer as it writes it, each observation as it is taken and a steering
/// message as it lands, all keyed by the run. A second writer in this loop
/// meant the same sentence reached the store twice as soon as the core
/// learned to stream.
fn steer(runtime: &Arc<Runtime>, state: &mut State, context: &mut Loop, run: RunId, text: String) {
    match runtime.steer(run, &text) {
        Ok(true) => state.note("steered into the running turn"),
        // The run settled between the keystroke and the call. What the user
        // typed is not lost: it becomes the next turn.
        Ok(false) => {
            state.steer_missed(&text);
            send(runtime, state, context, text);
        }
        Err(error) => {
            state.steer_missed(&text);
            state.note(format!("not steered: {error} — sending it as a new turn"));
            send(runtime, state, context, text);
        }
    }
}

#[allow(clippy::too_many_lines)]
fn execute(
    runtime: &Arc<Runtime>,
    terminal: &mut DefaultTerminal,
    state: &mut State,
    context: &mut Loop,
    command: Command,
) -> Result<(), TuiError> {
    match command {
        Command::Send { text } => send(runtime, state, context, text),
        Command::Steer { run, text } => steer(runtime, state, context, run, text),
        Command::Nav { options } => nav(runtime, state, context, options),
        Command::AppGoal { app, goal } => app_goal(runtime, state, context, app, goal),
        Command::Ax { request } => ax(runtime, state, context, request),
        Command::Eval { selection } => eval(runtime, state, context, selection),
        Command::EvalList => eval_list(state),
        Command::Doctor => match runtime.doctor() {
            Ok(report) => {
                let failures = report
                    .checks
                    .iter()
                    .filter(|check| check.health == neo_agent::doctor::Health::Fail)
                    .count();
                state.doctor = report;
                state.note(format!("doctor re-run · {failures} failing check(s)"));
            }
            Err(error) => state.note(format!("doctor: {error}")),
        },
        Command::StopRun { run } => {
            context.jobs.cancel(run);
        }
        Command::StopAll => context.jobs.cancel_all(),
        Command::StartDictation => start_dictation(runtime, state, &mut context.dictation),
        Command::StopDictation => {
            stop_dictation(state, &mut context.dictation, &mut context.voice);
        }
        Command::NewConversation { title } => match runtime.new_conversation(title) {
            Ok(conversation) => {
                state.load_thread(conversation.id, conversation.title, &[]);
                state.note("new conversation");
            }
            Err(error) => state.note(format!("the conversation was not created: {error}")),
        },
        Command::ListConversations => match runtime.conversations(SESSION_LIMIT) {
            Ok(conversations) => {
                let rows = conversations
                    .iter()
                    .map(|conversation| SessionRow {
                        id: conversation.id,
                        title: conversation
                            .title
                            .clone()
                            .unwrap_or_else(|| format!("untitled · {}", conversation.id)),
                        when: stamp(conversation.updated_at),
                        active: Some(conversation.id) == state.conversation,
                    })
                    .collect();
                state.show_sessions(rows);
            }
            Err(error) => state.note(format!("the conversations could not be listed: {error}")),
        },
        Command::SwitchConversation { conversation } => {
            match runtime.thread(conversation, THREAD_LIMIT) {
                Ok(messages) => {
                    let title = runtime
                        .conversations(SESSION_LIMIT)
                        .ok()
                        .and_then(|rows| rows.into_iter().find(|row| row.id == conversation))
                        .and_then(|row| row.title);
                    state.load_thread(conversation, title, &messages);
                    state.note("switched conversation");
                }
                Err(error) => state.note(format!("that thread could not be read: {error}")),
            }
        }
        Command::RenameConversation { title } => match state.conversation {
            Some(conversation) => match runtime.rename_conversation(conversation, &title) {
                Ok(()) => {
                    state.conversation_title = Some(title.clone());
                    state.note(format!("renamed to “{title}”"));
                }
                Err(error) => state.note(format!("not renamed: {error}")),
            },
            None => state.note("no conversation is open"),
        },
        Command::PatchSettings { section, patch } => match runtime.patch_settings(section, patch) {
            Ok(_) => state.note(format!("{section} saved")),
            Err(error) => state.note(format!("{section} rejected: {error}")),
        },
        // 04 §14's `set_key` contract is store-then-validate: the key is kept
        // either way, and an offline machine leaves it `unchecked` rather than
        // calling it bad (05 §6).
        Command::SetKey { account, raw } => match runtime.set_key(&account, &raw) {
            Ok(_) => {
                state.note(format!("{account} key stored — checking"));
                check_key(runtime, state, &account);
            }
            Err(error) => state.note(format!("{account} key rejected: {error}")),
        },
        Command::RemoveKey { account } => match runtime.remove_key(&account) {
            Ok(status) => state.note(format!("{account} key is {}", key_label(status.state))),
            Err(error) => state.note(format!("{account} key not removed: {error}")),
        },
        // A vendor login owns the terminal for as long as it takes (a browser
        // round-trip, a code paste), so the front end steps aside and comes
        // back: the alternate screen is left and re-entered around the call.
        Command::ConnectSubscription { provider } => {
            let outcome = with_terminal_released(terminal, || {
                tokio::runtime::Handle::current().block_on(connect(runtime, provider))
            });
            state.dirty = true;
            note_account(state, provider, outcome);
            state.rebootstrap(runtime.bootstrap()?);
        }
        Command::DisconnectSubscription { provider } => {
            let outcome = tokio::runtime::Handle::current().block_on(disconnect(runtime, provider));
            note_account(state, provider, outcome);
            state.rebootstrap(runtime.bootstrap()?);
        }
        // Starkbot's own login (K7): no terminal handover. The overlay goes
        // up immediately with the URL, and the wait runs in the background so
        // the paste fallback and Esc stay live.
        Command::BeginLogin { provider } => {
            let Some(provider) = oauth_provider(provider) else {
                state.note(format!("{provider} does not sign in through Starkbot"));
                return Ok(());
            };
            match LoginWorker::start(runtime, provider) {
                Ok((worker, url)) => {
                    state.login = Some(Login {
                        provider: provider.id,
                        title: plan_title(provider.id),
                        url,
                        redirect_uri: worker.handle.redirect_uri().to_owned(),
                        phase: LoginPhase::Waiting,
                        paste: String::new(),
                        remaining: worker.seconds_left(),
                    });
                    context.login = Some(worker);
                    state.dirty = true;
                }
                Err(error) => state.note(format!("could not start the login: {error}")),
            }
        }
        Command::OpenLoginPage => match (context.login.as_mut(), state.login.as_ref()) {
            (Some(worker), Some(overlay)) => {
                let url = overlay.url.clone();
                worker.spawn_open(runtime, &url);
                state.note("opening the vendor page again");
            }
            _ => state.note("no login is waiting"),
        },
        Command::FinishLoginPasted { pasted } => match context.login.as_mut() {
            Some(worker) => worker.spawn_paste(runtime, pasted),
            None => state.note("no login is waiting"),
        },
        // Dropping the worker aborts the wait, which releases the port.
        Command::CancelLogin => {
            context.login = None;
            state.note("login cancelled");
        }
        Command::RefreshModels { account } => {
            let outcome =
                tokio::runtime::Handle::current().block_on(runtime.refresh_models(&account));
            match outcome {
                Ok(models) => state.note(format!("{account}: {} models", models.len())),
                Err(error) => state.note(format!("{account}: {error}")),
            }
        }
        Command::CheckKey { account } => check_key(runtime, state, &account),
        // `Ctrl-L` is for a screen something else wrote over, so the
        // display and ratatui's back buffer disagree. Clearing resets both
        // — the diff the next frame computes is against a blank buffer, so
        // the whole UI is emitted again.
        Command::Redraw => {
            terminal.clear()?;
            state.dirty = true;
        }
        Command::ReBootstrap => rebootstrap(runtime, state)?,
    }
    Ok(())
}

/// One agent turn.
///
/// The run id is minted here, before the turn starts, so the pane can show it
/// and the reducer can filter by it the moment the first `TurnStarted`
/// arrives — a run id handed back at the end is useless to something that
/// wanted to watch the middle.
fn send(runtime: &Arc<Runtime>, state: &mut State, context: &mut Loop, text: String) {
    let Some(conversation) = state.conversation else {
        state.note("no conversation is open — nothing was sent");
        return;
    };
    let message = ChatMessage::user(&text);
    // Recorded before the turn so the model's history and the thread on
    // screen are the same list; the row appears from the resulting event.
    if let Err(error) = runtime.record_message(conversation, &message, false) {
        state.note(format!("the message was not saved: {error}"));
    }
    let mut history = state.history();
    history.push(message);

    // The request mints the id, and it does so before the turn starts, so
    // the pane can show the run and the reducer can filter by it from the
    // very first `TurnStarted`.
    let request = ChatRequest::new(conversation, history);
    let run = request.run;
    state.start_run(run, RunKind::Chat, one_line(&text));
    let handle = Arc::clone(runtime);
    context.jobs.spawn(run, move |cancel| async move {
        match handle.chat(request.with_cancel(cancel)).await {
            Ok(outcome) => Ok((Vec::new(), outcome.text)),
            Err(error) => Err(error.to_string()),
        }
    });
}

fn nav(runtime: &Arc<Runtime>, state: &mut State, context: &mut Loop, spec: NavSpec) {
    let settings = match runtime.settings() {
        Ok(settings) => settings,
        Err(error) => {
            state.note(format!("settings: {error}"));
            return;
        }
    };
    let mut options = BrowserOptions::unattended(&settings, spec.url.clone(), spec.goal.clone());
    options.headed = spec.headed;
    options.safety_heads = spec.safety_heads;
    options.profile = spec.profile.map(PathBuf::from);

    let run = RunId::new();
    state.start_run(run, RunKind::Nav, format!("{} — {}", spec.url, spec.goal));
    if !spec.safety_heads {
        state.note("--no-safety: no head is asked, so nothing can trip the confirm gate");
    }
    let handle = Arc::clone(runtime);
    context.jobs.spawn(run, move |cancel| async move {
        match run_browser(&handle, &options, run, &cancel).await {
            Ok(finished) => Ok((
                Vec::new(),
                format!(
                    "{:?} · {} step(s) · {} ms — {}",
                    finished.outcome, finished.steps, finished.duration_ms, finished.observation
                ),
            )),
            Err(error) => Err(error.to_string()),
        }
    });
}

fn app_goal(
    runtime: &Arc<Runtime>,
    state: &mut State,
    context: &mut Loop,
    app: String,
    goal: String,
) {
    let settings = match runtime.settings() {
        Ok(settings) => settings,
        Err(error) => {
            state.note(format!("settings: {error}"));
            return;
        }
    };
    let options = AppOptions::unattended(&settings, app.clone(), goal.clone());
    let run = RunId::new();
    state.start_run(run, RunKind::App, format!("{app} — {goal}"));
    let handle = Arc::clone(runtime);
    context.jobs.spawn(run, move |cancel| async move {
        match run_app(&handle, &options, run, &cancel).await {
            Ok(finished) => Ok((
                Vec::new(),
                format!(
                    "{:?} · {} step(s) · {} ms — {}",
                    finished.outcome, finished.steps, finished.duration_ms, finished.observation
                ),
            )),
            Err(error) => Err(error.to_string()),
        }
    });
}

/// One direct accessibility call.
///
/// The answer is rendered as the JSON `neo ax` prints, because that is the
/// shape scripts already read and a second, terminal-only rendering of an
/// element table would be a fork of the contract. A column view is a later
/// job, not a different answer.
fn ax(runtime: &Arc<Runtime>, state: &mut State, context: &mut Loop, request: AxRequest) {
    let run = RunId::new();
    state.start_run(run, RunKind::Ax, ax_title(&request));
    let handle = Arc::clone(runtime);
    context.jobs.spawn(run, move |cancel| async move {
        match neo_agent::ax::ax(&handle, request, run, &cancel).await {
            Ok(response) => {
                let summary = ax_summary(&response);
                let detail = serde_json::to_string_pretty(&response)
                    .unwrap_or_else(|error| format!("(unprintable: {error})"))
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect();
                Ok((detail, summary))
            }
            Err(error) => Err(error.to_string()),
        }
    });
}

fn ax_title(request: &AxRequest) -> String {
    match request {
        AxRequest::Trusted => "ax trusted".to_owned(),
        AxRequest::Apps => "ax apps".to_owned(),
        AxRequest::Table { app } => format!("ax table {app}"),
        AxRequest::Press { app, index } => format!("ax press {app} [{index}]"),
        // The text is counted, never titled: a field being typed into may
        // hold a one-time code, and a run row is on screen for minutes.
        AxRequest::Set { app, index, text } => {
            format!("ax set {app} [{index}] · {} chars", text.chars().count())
        }
        AxRequest::Menu { app, path } => format!("ax menu {app} {path}"),
        AxRequest::Type { app, text } => format!("ax type {app} · {} chars", text.chars().count()),
        AxRequest::Key { app, key } => format!("ax key {app} {key}"),
    }
}

fn ax_summary(response: &AxResponse) -> String {
    match response {
        AxResponse::Trust(report) => format!(
            "trusted {} · can post events {}",
            report.trusted, report.can_post_events
        ),
        AxResponse::Apps(apps) => format!("{} application(s)", apps.len()),
        AxResponse::Acted(report) => format!(
            "{} via {}{} — {}",
            if report.performed {
                "performed"
            } else {
                "refused"
            },
            report.method,
            if report.relocated { " (relocated)" } else { "" },
            report.summary
        ),
        AxResponse::Table(table) => format!(
            "{} element(s) · {} control(s) · generation {}",
            table.elements.len(),
            table.controls.len(),
            table.generation
        ),
    }
}

fn eval(runtime: &Arc<Runtime>, state: &mut State, context: &mut Loop, selection: Selection) {
    let run = RunId::new();
    let title = format!(
        "suite{}{}{}",
        selection
            .filter
            .as_deref()
            .map(|filter| format!(" · filter {filter}"))
            .unwrap_or_default(),
        if selection.tags.is_empty() {
            String::new()
        } else {
            format!(" · tags {}", selection.tags.join(","))
        },
        if selection.once { " · once" } else { "" }
    );
    state.start_run(run, RunKind::Eval, title);
    state.note("one case at a time — they share the keyboard and the frontmost app");
    let handle = Arc::clone(runtime);
    context.jobs.spawn(run, move |cancel| async move {
        match neo_eval::run_suite(&handle, selection, run, &cancel).await {
            Ok(report) => {
                let detail = report
                    .tests
                    .iter()
                    .map(|test| {
                        format!(
                            "{} {}",
                            if test.passed { "pass" } else { "FAIL" },
                            test.test_id
                        )
                    })
                    .collect();
                Ok((
                    detail,
                    format!(
                        "{} of {} passed · {} failed",
                        report.passed, report.total, report.failed
                    ),
                ))
            }
            Err(error) => Err(error.to_string()),
        }
    });
}

/// What `neo eval --list` prints: every case and whether this machine can run
/// it, without spending a token. Not a run, so it goes to the event ring the
/// Mind pane falls back to rather than into the runs list.
fn eval_list(state: &mut State) {
    for (app, installed) in neo_eval::availability() {
        state.note(format!(
            "{} {}",
            app.label(),
            installed.map_or_else(
                || "not installed — its cases are skipped".to_owned(),
                |path| path.display().to_string()
            )
        ));
    }
    let cases = neo_eval::list_cases();
    for case in &cases {
        state.note(format!(
            "{} {} [{}]",
            if case.runnable() { "run " } else { "skip" },
            case.id,
            case.tags.join(" ")
        ));
    }
    state.note(format!("{} case(s) — `:eval` runs them", cases.len()));
}

/// A stored timestamp as a date and a minute, which is what a switcher row
/// needs. Nothing finer: a conversation list is not a log.
///
/// UTC, and labelled UTC. Reading the machine's zone needs `time`'s
/// `local-offset` feature, which is unsound in a process with threads — and
/// this one has several — so a wrong-by-an-hour local time would be worse
/// than an honest one.
fn stamp(at: neo_core::TimestampMs) -> String {
    let Ok(moment) = time::OffsetDateTime::from_unix_timestamp(at / 1000) else {
        return "unknown".to_owned();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        moment.year(),
        u8::from(moment.month()),
        moment.day(),
        moment.hour(),
        moment.minute()
    )
}

/// One line of a message, for a run row that has to fit on one.
fn one_line(text: &str) -> String {
    let flat: String = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= 72 {
        return flat;
    }
    format!("{}…", flat.chars().take(71).collect::<String>())
}

/// The `OauthProvider` a row's provider id names, if it is one of the two.
fn oauth_provider(provider: &str) -> Option<&'static OauthProvider> {
    if provider == ANTHROPIC_OAUTH.id {
        Some(&ANTHROPIC_OAUTH)
    } else if provider == OPENAI_CODEX.id {
        Some(&OPENAI_CODEX)
    } else {
        None
    }
}

async fn connect(
    runtime: &Runtime,
    provider: &'static str,
) -> Result<neo_core::ProviderAccount, RuntimeError> {
    match provider {
        neo_core::PROVIDER_CLAUDE_SUBSCRIPTION => runtime.connect_claude().await,
        other => Err(RuntimeError::RuntimeUnavailable(other.to_owned())),
    }
}

async fn disconnect(
    runtime: &Runtime,
    provider: &'static str,
) -> Result<neo_core::ProviderAccount, RuntimeError> {
    match provider {
        neo_core::PROVIDER_CLAUDE_SUBSCRIPTION => runtime.disconnect_claude().await,
        other => Err(RuntimeError::RuntimeUnavailable(other.to_owned())),
    }
}

fn note_account(
    state: &mut State,
    provider: &str,
    outcome: Result<neo_core::ProviderAccount, RuntimeError>,
) {
    match outcome {
        Ok(account) => state.note(format!("{provider} is {:?}", account.status)),
        Err(error) => state.note(format!("{provider}: {error}")),
    }
}

/// Leave the alternate screen, run `body`, and take the terminal back.
///
/// Restoring first means the vendor's own prompts and its browser handoff
/// are visible. Coming back is the half that has to be done deliberately.
/// This used to call `ratatui::try_init()` and throw away the
/// `DefaultTerminal` it returned, which broke the screen twice over:
/// re-entering the alternate buffer blanks the display while the terminal
/// the loop still draws through holds the pre-handover frame in its back
/// buffer, so the next `draw` writes only a diff against a frame nobody can
/// see and most of the UI never repaints — and `try_init` installs a panic
/// hook chained onto the previous one, so every login left another copy
/// behind.
///
/// So the screen is re-entered with the two crossterm calls `try_init`
/// would have made, the terminal this front end already owns is kept, and
/// `Terminal::clear` resets its back buffer so the next
/// frame is drawn whole. The panic hook installed once by [`TerminalGuard`]
/// stays the only one.
fn with_terminal_released<T>(terminal: &mut DefaultTerminal, body: impl FnOnce() -> T) -> T {
    if let Err(error) = ratatui::try_restore() {
        tracing::warn!(%error, "could not hand the terminal over");
    }
    let outcome = body();
    if let Err(error) = reclaim_terminal(terminal) {
        tracing::warn!(%error, "could not take the terminal back");
    }
    outcome
}

/// Re-enter the alternate screen and make the next frame a full repaint.
fn reclaim_terminal(terminal: &mut DefaultTerminal) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()
}

/// Ask the vendor what a stored key is worth. `Runtime::check_key` is async
/// and this loop is not, so the one await point in the front end lives here,
/// on the runtime handle the `neo tui` command entered through
/// `block_in_place`.
fn check_key(runtime: &Runtime, state: &mut State, account: &str) {
    let checked = tokio::runtime::Handle::current().block_on(runtime.check_key(account));
    match checked {
        Ok(status) => state.note(format!("{account} key is {}", key_label(status.state))),
        Err(error) => state.note(format!("{account} key not checked: {error}")),
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    /// The real thing, in the real shape: a chat turn spawned as a job from
    /// inside `block_in_place`, which is how the frame loop starts every
    /// turn. A turn that works from the CLI and hangs here is the difference
    /// between `neo ask` and the TUI, and this is the smallest harness that
    /// can tell them apart.
    ///
    /// Live: it spends a real turn on the selected runtime.
    /// `cargo test -p neo-tui --lib a_chat_turn_settles -- --ignored --nocapture`
    #[test]
    #[ignore = "spends a real inference turn on the selected runtime"]
    fn a_chat_turn_spawned_from_the_frame_loop_settles() {
        let data_dir = dirs_data_dir();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("a multi-thread runtime");
        runtime.block_on(async {
            let core = Arc::new(
                tokio::task::block_in_place(|| Runtime::open(&data_dir)).expect("the store opens"),
            );
            tokio::task::block_in_place(|| {
                let conversation = core.open_conversation().expect("a conversation");
                let mut jobs = Jobs::new();
                let request = neo_agent::agent::ChatRequest::new(
                    conversation.id,
                    vec![ChatMessage::user("reply with exactly: spawned ok")],
                );
                let run = request.run;
                let handle = Arc::clone(&core);
                jobs.spawn(run, move |cancel| async move {
                    match handle.chat(request.with_cancel(cancel)).await {
                        Ok(outcome) => Ok((Vec::new(), outcome.text)),
                        Err(error) => Err(error.to_string()),
                    }
                });
                let deadline = Instant::now() + Duration::from_secs(90);
                loop {
                    match jobs.settled.try_recv() {
                        Ok(settled) => {
                            // `print_stdout` is denied for this crate, and a
                            // live test that printed into the alternate screen
                            // would be unreadable anyway: the assertion is the
                            // report.
                            assert!(
                                settled.outcome.is_ok(),
                                "the turn failed: {:?}",
                                settled.outcome
                            );
                            return;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => {
                            assert!(Instant::now() < deadline, "the turn never settled in 90 s");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            panic!("the turn's channel closed without a result")
                        }
                    }
                }
            });
        });
    }

    /// The same data directory the CLI uses, so the test sees the real
    /// credentials and the real selected runtime.
    fn dirs_data_dir() -> std::path::PathBuf {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
            .join("Library/Application Support/com.starkbot.neo")
    }

    /// The frame loop runs inside `block_in_place`, and every unit of work the
    /// TUI starts is a `tokio::spawn` from inside it. If a job spawned that
    /// way is never polled, the pane shows "running" for ever and nothing on
    /// screen says why — which is exactly how a hung turn presents. This pins
    /// the plumbing under the same threading model the real loop uses.
    #[test]
    fn a_job_spawned_from_inside_block_in_place_settles() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a multi-thread runtime");
        runtime.block_on(async {
            tokio::task::block_in_place(|| {
                let mut jobs = Jobs::new();
                let run = RunId::new();
                jobs.spawn(run, |_cancel| async move {
                    Ok((vec!["worked".to_owned()], "done".to_owned()))
                });
                // The frame loop polls with `try_recv`, so the test waits the
                // way the loop does rather than awaiting the channel.
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match jobs.settled.try_recv() {
                        Ok(settled) => {
                            assert_eq!(settled.run, run);
                            assert_eq!(settled.outcome, Ok("done".to_owned()));
                            return;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => {
                            assert!(
                                Instant::now() < deadline,
                                "the job never settled: a task spawned from inside \
                                 `block_in_place` was not polled"
                            );
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            panic!("the job's channel closed without a result")
                        }
                    }
                }
            });
        });
    }
}
