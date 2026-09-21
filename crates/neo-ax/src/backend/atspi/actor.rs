//! The AT-SPI actor: one thread, one runtime, one command at a time.
//!
//! An AT-SPI object is a pair of strings, so unlike `AXUIElement` nothing
//! here is pinned to a thread by the API. The actor exists anyway, and for
//! the same three reasons the macOS one does:
//!
//! * **One command at a time.** A walk and an action interleaved on one
//!   app's single-threaded toolkit is how a tree is read half-updated.
//! * **Generations.** A `Ref` is an index into one observation, and the
//!   store that resolves it lives here and nowhere else.
//! * **A kill switch that is not a message.** `stop` is an `AtomicBool`
//!   read between commands and between typed pages, so it lands inside one
//!   page even while a long string is going in.
//!
//! The thread owns a current-thread tokio runtime because `zbus`'s tokio
//! transport needs a reactor, and the command channel is
//! `tokio::sync::mpsc` rather than `std::sync::mpsc` for the same reason: a
//! blocking `recv` would stop the runtime the D-Bus socket is driven by.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use atspi::{Role, State};
use futures_util::FutureExt;
use tokio::sync::{mpsc, oneshot};

use super::bus::{AxRef, Bus};
use super::input::VirtualInput;
use super::walk::{Walk, Walker};
use super::{apps, perm, wm};
use crate::deny::AxPolicy;
use crate::error::{AxError, Freshness, StaleReason};
use crate::raw::RawNode;
use crate::table::{self, BuiltTable, TableInput};
use crate::types::{
    ActOutcome, AppInfo, AppSel, AxAction, Element, ElementTable, Fingerprint, Guard, Key, Method,
    Modifier, Rect, Ref, ScrollDir,
};

/// Wall deadline for one element table (01 §Messaging timeouts).
const TABLE_DEADLINE: Duration = Duration::from_millis(400);
/// How long an app that just came forward is given to publish a window.
const WINDOW_WAIT: Duration = Duration::from_millis(1_500);
/// How long an app is given to take focus after it is asked to.
const ACTIVATE_DEADLINE: Duration = Duration::from_millis(3_000);
/// How long a guard waits for a window it just re-raised. Far shorter than
/// [`ACTIVATE_DEADLINE`]: this is paid on a step that is otherwise ready to
/// act, not on the one-off that starts a run.
const REFOCUS_DEADLINE: Duration = Duration::from_millis(250);
/// How long a just-launched app is given to appear at all.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(8);
/// How long a written value is given to appear in the element.
const TYPED_VALUE_WAIT: Duration = Duration::from_millis(600);
/// How many generations of refs stay alive.
const GENERATIONS_KEPT: usize = 2;
/// Action names that mean "press this", in the order they are tried.
///
/// AT-SPI has no fixed vocabulary and every toolkit picked its own:
/// LibreOffice and WebKitGTK say `press`, Chromium says `click`, GTK4 says
/// `activate`. An object that offers an action nobody here recognises is
/// still pressed — by index 0 — because refusing to press a button whose
/// verb is spelled differently helps nobody.
const PRESS_VERBS: [&str; 7] = [
    "click",
    "press",
    "activate",
    "jump",
    "open",
    "toggle",
    "expand or contract",
];

type Reply<T> = oneshot::Sender<Result<T, AxError>>;

enum AxCmd {
    Apps(Reply<Vec<AppInfo>>),
    Activate {
        app: AppSel,
        reply: Reply<AppInfo>,
    },
    Table {
        app: AppSel,
        goal: Option<String>,
        reply: Reply<ElementTable>,
    },
    Guard {
        guard: Box<Guard>,
        reply: Reply<Freshness>,
    },
    Act {
        action: Box<AxAction>,
        reply: Reply<ActOutcome>,
    },
    Shutdown,
}

impl AxCmd {
    const fn name(&self) -> &'static str {
        match self {
            Self::Apps(_) => "apps",
            Self::Activate { .. } => "activate",
            Self::Table { .. } => "table",
            Self::Guard { .. } => "guard",
            Self::Act { .. } => "act",
            Self::Shutdown => "shutdown",
        }
    }

    /// Answer without doing the work, because the actor has been stopped.
    fn refuse(self) {
        match self {
            Self::Apps(reply) => {
                let _ = reply.send(Err(AxError::Stopped));
            }
            Self::Activate { reply, .. } => {
                let _ = reply.send(Err(AxError::Stopped));
            }
            Self::Table { reply, .. } => {
                let _ = reply.send(Err(AxError::Stopped));
            }
            Self::Guard { reply, .. } => {
                let _ = reply.send(Err(AxError::Stopped));
            }
            Self::Act { reply, .. } => {
                let _ = reply.send(Err(AxError::Stopped));
            }
            Self::Shutdown => {}
        }
    }
}

/// A `Clone + Send + Sync` handle onto the AT-SPI actor.
///
/// Cloning is cheap and every clone talks to the same actor. Dropping the
/// last clone shuts the thread down.
#[derive(Clone)]
pub struct AxHandle {
    tx: mpsc::UnboundedSender<AxCmd>,
    kill: Arc<AtomicBool>,
    locked: Arc<AtomicBool>,
}

impl AxHandle {
    /// Start the actor thread.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBus`] when the session publishes no accessibility bus,
    /// [`AxError::NoWindowManager`] when no compositor here can be driven,
    /// and [`AxError::ActorDead`] when the OS refuses the thread. None of
    /// them is a permission: there is none to ask for (see [`perm`]).
    pub fn spawn() -> Result<Self, AxError> {
        Self::spawn_with_policy(AxPolicy::new(std::process::id().cast_signed()))
    }

    /// Start the actor thread with a tightened deny list.
    ///
    /// # Errors
    ///
    /// As [`AxHandle::spawn`].
    pub fn spawn_with_policy(policy: AxPolicy) -> Result<Self, AxError> {
        let (tx, rx) = mpsc::unbounded_channel::<AxCmd>();
        let kill = Arc::new(AtomicBool::new(false));
        let locked = Arc::new(AtomicBool::new(false));
        let thread_kill = Arc::clone(&kill);
        let thread_locked = Arc::clone(&locked);
        // The two things that can be missing are checked on the calling
        // thread so the caller learns which one it was, rather than getting
        // a handle that fails every later command.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), AxError>>();

        std::thread::Builder::new()
            .name("neo-ax".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = ready_tx.send(Err(AxError::ActorDead));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let started = Actor::new(policy, thread_kill, thread_locked).await;
                    match started {
                        Ok(mut actor) => {
                            let _ = ready_tx.send(Ok(()));
                            actor.run(rx).await;
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                        }
                    }
                });
            })
            .map_err(|_| AxError::ActorDead)?;

        ready_rx.recv().map_err(|_| AxError::ActorDead)??;
        Ok(Self { tx, kill, locked })
    }

    /// `true`: no accessibility grant gates AT-SPI2 (17 §3.4).
    #[must_use]
    pub fn trusted() -> bool {
        perm::trusted()
    }

    /// `true`, with nothing asked for: there is no prompt to raise.
    #[must_use]
    pub fn request_trust() -> bool {
        perm::request_trust()
    }

    /// Tell the actor whether the session is locked. No event is ever posted
    /// into a lock screen (P7 backstop).
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
    }

    /// Stop this actor, for good.
    ///
    /// Lock-free and non-blocking: the flag is read between commands and
    /// between typed pages, so a stop lands within one page even while a
    /// long string is going in. Sticky on purpose, as on macOS — a run that
    /// was stopped starts a fresh handle rather than inheriting a half-typed
    /// field and a generation of refs taken before the user intervened.
    pub fn stop(&self) {
        self.kill.store(true, Ordering::SeqCst);
    }

    /// Whether [`AxHandle::stop`] has been called.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.kill.load(Ordering::SeqCst)
    }

    /// Every application that owns a window.
    ///
    /// # Errors
    ///
    /// Fails when the actor thread is gone or the compositor cannot be
    /// reached.
    pub async fn apps(&self) -> Result<Vec<AppInfo>, AxError> {
        self.call(AxCmd::Apps).await
    }

    /// Bring an app to the front and wait for it to get there.
    ///
    /// # Errors
    ///
    /// Fails when no app matches and none can be launched, when the app is
    /// denied, or when the compositor will not focus it.
    pub async fn activate(&self, app: &AppSel) -> Result<AppInfo, AxError> {
        let app = app.clone();
        self.call(|reply| AxCmd::Activate { app, reply }).await
    }

    /// One observation of an app: the element table of 01 §Element table.
    ///
    /// # Errors
    ///
    /// Fails on a denied app, on a Chromium-family browser (the web path
    /// owns those), when the app has no window, and when it publishes no
    /// accessibility tree.
    pub async fn table(&self, app: &AppSel) -> Result<ElementTable, AxError> {
        self.table_for_goal(app, None).await
    }

    /// Like [`AxHandle::table`], ranking menu leaves by overlap with the
    /// goal so the ones worth offering survive the 250-row budget.
    ///
    /// # Errors
    ///
    /// As [`AxHandle::table`].
    pub async fn table_for_goal(
        &self,
        app: &AppSel,
        goal: Option<&str>,
    ) -> Result<ElementTable, AxError> {
        let app = app.clone();
        let goal = goal.map(ToOwned::to_owned);
        self.call(|reply| AxCmd::Table { app, goal, reply }).await
    }

    /// Is the surface still the one the decision was made on?
    ///
    /// # Errors
    ///
    /// Fails when the actor thread is gone. A failed *check* is
    /// [`Freshness::Stale`], not an error.
    pub async fn guard(&self, guard: &Guard) -> Result<Freshness, AxError> {
        let guard = Box::new(guard.clone());
        self.call(|reply| AxCmd::Guard { guard, reply }).await
    }

    /// Execute one chosen action, AT-SPI first.
    ///
    /// # Errors
    ///
    /// Fails on a stale ref, a denied app, a secure field, a locked screen,
    /// and when neither an action nor the virtual-input fallback can carry
    /// it out.
    pub async fn act(&self, action: &AxAction) -> Result<ActOutcome, AxError> {
        let action = Box::new(action.clone());
        self.call(|reply| AxCmd::Act { action, reply }).await
    }

    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> AxCmd) -> Result<T, AxError> {
        if self.is_stopped() {
            return Err(AxError::Stopped);
        }
        let (tx, rx) = oneshot::channel();
        self.tx.send(make(tx)).map_err(|_| AxError::ActorDead)?;
        rx.await.map_err(|_| AxError::ActorDead)?
    }
}

impl Drop for AxHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(AxCmd::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// One observation's worth of live objects, kept on the actor thread.
struct Generation {
    id: u32,
    pid: i32,
    /// Walk-order element store; `RawNode::id` indexes it.
    store: Vec<AxRef>,
    /// Element index -> slot in `store`.
    slots: Vec<u32>,
    fingerprints: Vec<Fingerprint>,
    elements: Vec<Element>,
    /// Frame of the largest scroll area, whose height sets the scroll step.
    scroll_area: Option<Rect>,
    /// Menu paths by element index, for `MENU` rows.
    menu_paths: Vec<(u16, Vec<String>)>,
    /// The window this observation described.
    window: AxRef,
}

struct Actor {
    bus: Bus,
    wm: Box<dyn wm::WindowManager>,
    input: Option<VirtualInput>,
    policy: AxPolicy,
    next_generation: u32,
    generations: VecDeque<Generation>,
    kill: Arc<AtomicBool>,
    locked: Arc<AtomicBool>,
}

impl Actor {
    async fn new(
        policy: AxPolicy,
        kill: Arc<AtomicBool>,
        locked: Arc<AtomicBool>,
    ) -> Result<Self, AxError> {
        let bus = Bus::connect().await?;
        let wm = wm::detect()?;
        Ok(Self {
            bus,
            wm,
            input: None,
            policy,
            next_generation: 1,
            generations: VecDeque::new(),
            kill,
            locked,
        })
    }

    async fn run(&mut self, mut rx: mpsc::UnboundedReceiver<AxCmd>) {
        while let Some(cmd) = rx.recv().await {
            if matches!(cmd, AxCmd::Shutdown) {
                return;
            }
            if self.kill.load(Ordering::SeqCst) {
                cmd.refuse();
                continue;
            }
            self.dispatch(cmd).await;
        }
    }

    async fn dispatch(&mut self, cmd: AxCmd) {
        let name = cmd.name();
        match cmd {
            AxCmd::Apps(reply) => {
                let out = self.protected(name, self.cmd_apps()).await;
                let _ = reply.send(out);
            }
            AxCmd::Activate { app, reply } => {
                let out = AssertUnwindSafe(self.cmd_activate(&app))
                    .catch_unwind()
                    .await;
                let _ = reply.send(unwind(name, out));
            }
            AxCmd::Table { app, goal, reply } => {
                let out = AssertUnwindSafe(self.cmd_table(&app, goal.as_deref()))
                    .catch_unwind()
                    .await;
                let out = unwind(name, out);
                if out.is_err() && matches!(out, Err(AxError::Panic { .. })) {
                    self.invalidate_all();
                }
                let _ = reply.send(out);
            }
            AxCmd::Guard { guard, reply } => {
                let out = AssertUnwindSafe(self.cmd_guard(&guard))
                    .catch_unwind()
                    .await;
                let _ = reply.send(unwind(name, out.map(Ok)));
            }
            AxCmd::Act { action, reply } => {
                let out = AssertUnwindSafe(self.cmd_act(&action)).catch_unwind().await;
                let _ = reply.send(unwind(name, out));
            }
            AxCmd::Shutdown => {}
        }
    }

    async fn protected<T>(
        &self,
        name: &'static str,
        body: impl Future<Output = Result<T, AxError>>,
    ) -> Result<T, AxError> {
        unwind(name, AssertUnwindSafe(body).catch_unwind().await)
    }

    /// Every outstanding ref dies.
    fn invalidate_all(&mut self) {
        self.generations.clear();
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
    }

    // -- commands ----------------------------------------------------------

    async fn cmd_apps(&self) -> Result<Vec<AppInfo>, AxError> {
        Ok(apps::running(self.wm.as_ref())
            .into_iter()
            .filter(|a| !self.policy.is_denied(a))
            .collect())
    }

    /// The app a selector names, with the deny list applied.
    ///
    /// A name that matched no application name gets one more pass over the
    /// *window titles*, which 17 §3.2 asks for and `AppSel::matches` cannot
    /// do on its own: "the spreadsheet" is in the title bar long before it
    /// is in a desktop entry.
    fn resolve_app(&self, sel: &AppSel) -> Result<AppInfo, AxError> {
        let all = apps::running(self.wm.as_ref());
        let found = sel
            .pick(&all)
            .cloned()
            .or_else(|| self.by_window_title(sel, &all))
            .ok_or_else(|| AxError::NoApp {
                selector: sel.to_string(),
            })?;
        if self.policy.is_denied(&found) {
            return Err(AxError::Denied {
                app: found.bundle_id.clone().unwrap_or(found.name.clone()),
            });
        }
        Ok(found)
    }

    fn by_window_title(&self, sel: &AppSel, all: &[AppInfo]) -> Option<AppInfo> {
        let AppSel::Name(needle) = sel else {
            return None;
        };
        let needle = needle.to_lowercase();
        if needle.len() < 3 {
            return None;
        }
        let clients = self.wm.clients().ok()?;
        let hit = clients
            .iter()
            .find(|c| c.title.to_lowercase().contains(&needle))?;
        all.iter().find(|a| a.pid == hit.pid).cloned()
    }

    async fn wait_for_app(&self, sel: &AppSel) -> Result<AppInfo, AxError> {
        let deadline = Instant::now() + LAUNCH_DEADLINE;
        loop {
            if let Ok(app) = self.resolve_app(sel) {
                return Ok(app);
            }
            if Instant::now() >= deadline {
                return Err(AxError::NoApp {
                    selector: sel.to_string(),
                });
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }

    async fn cmd_activate(&mut self, sel: &AppSel) -> Result<AppInfo, AxError> {
        let app = match self.resolve_app(sel) {
            Ok(app) => app,
            // "Activate first" is the first input rule, and an app that is
            // not running cannot be activated. A name or an `app_id` can
            // still be launched from its desktop entry — but only after the
            // deny list has cleared it. A pid that is gone stays gone.
            // Starting an application is seat-taking too, whatever this crate
            // does afterwards: a window that has just mapped takes focus from
            // the compositor itself, and on one that warps on focus it takes
            // the cursor with it. So an app that is not already running is
            // refused rather than started behind somebody's work.
            Err(AxError::NoApp { .. }) if !crate::seat::may_take_seat() => {
                return Err(AxError::Unsupported(
                    "that application has no window open, and starting one would raise it over \
                     whatever is in front. If it ships a routine, use that — a routine reaches \
                     the application's own control channel and needs no window at all. \
                     Otherwise open it by hand, or set NEO_TAKE_SEAT=1 on a machine nobody is at",
                ));
            }
            Err(AxError::NoApp { .. }) => {
                let key = match sel {
                    AppSel::Name(name) => name.clone(),
                    AppSel::BundleId(id) => id.clone(),
                    AppSel::Pid(_) | AppSel::Frontmost => {
                        return Err(AxError::NoApp {
                            selector: sel.to_string(),
                        });
                    }
                };
                let entry = apps::find_entry(&key).ok_or(AxError::NoApp {
                    selector: sel.to_string(),
                })?;
                let probe = AppInfo {
                    name: entry.name.clone(),
                    bundle_id: Some(entry.id.clone()),
                    pid: -1,
                    frontmost: false,
                };
                if self.policy.is_denied(&probe) {
                    return Err(AxError::Denied {
                        app: sel.to_string(),
                    });
                }
                if !apps::launch(&entry) {
                    return Err(AxError::NoApp {
                        selector: sel.to_string(),
                    });
                }
                self.wait_for_app(sel).await?
            }
            Err(other) => return Err(other),
        };

        // An app that restarted has a new pid: nothing observed before it
        // came back may be acted on.
        let live: Vec<i32> = apps::running(self.wm.as_ref())
            .iter()
            .map(|a| a.pid)
            .collect();
        self.generations.retain(|g| live.contains(&g.pid));

        let deadline = Instant::now() + ACTIVATE_DEADLINE;
        loop {
            // Raising a window takes the person's typing target, and on a
            // compositor that warps on focus it takes their cursor too. So a
            // run only asks for it when it has been told it may.
            if crate::seat::may_take_seat()
                && let Some(client) = self.client_for(app.pid)
            {
                self.wm.focus(&client)?;
            }
            if !crate::seat::may_take_seat() {
                // Resolved, not raised: everything that does not need the
                // seat works against a window exactly where it is.
                return self.app_by_pid(app.pid).ok_or(AxError::NoApp {
                    selector: sel.to_string(),
                });
            }
            if self.is_frontmost(app.pid)
                && let Some(front) = self.app_by_pid(app.pid)
            {
                return Ok(front);
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        self.app_by_pid(app.pid).ok_or(AxError::NoApp {
            selector: sel.to_string(),
        })
    }

    /// Bring `pid` forward again, briefly, without the full resolve
    /// [`Self::cmd_activate`] does.
    ///
    /// Bounded on purpose: this runs inside a guard, which every step calls,
    /// and a window that does not come forward in a couple of frames is one
    /// the compositor is refusing to raise — that is a real stale surface and
    /// must be reported as one rather than waited on.
    async fn refocus(&mut self, pid: i32) {
        if !crate::seat::may_take_seat() {
            return;
        }
        let Some(client) = self.client_for(pid) else {
            return;
        };
        if self.wm.focus(&client).is_err() {
            return;
        }
        let deadline = Instant::now() + REFOCUS_DEADLINE;
        while !self.is_frontmost(pid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn app_by_pid(&self, pid: i32) -> Option<AppInfo> {
        apps::running(self.wm.as_ref())
            .into_iter()
            .find(|a| a.pid == pid)
    }

    /// The window of `pid` to drive: its active one, else the first.
    fn client_for(&self, pid: i32) -> Option<wm::Client> {
        let clients = self.wm.clients().ok()?;
        let active = self.wm.frontmost().ok().flatten();
        if let Some(active) = &active
            && active.pid == pid
        {
            return Some(active.clone());
        }
        clients.into_iter().find(|c| c.pid == pid)
    }

    fn is_frontmost(&self, pid: i32) -> bool {
        self.wm
            .frontmost()
            .ok()
            .flatten()
            .is_some_and(|c| c.pid == pid)
    }

    /// The AT-SPI application object for a pid.
    async fn app_root(&self, pid: i32) -> Option<AxRef> {
        let root = AxRef::registry_root();
        for child in self.bus.children(&root).await {
            if self.bus.pid_of(&child.bus).await == Some(pid.unsigned_abs()) {
                return Some(child);
            }
        }
        None
    }

    /// The frame to read, and the dialog in front of it when there is one.
    ///
    /// AT-SPI publishes a dialog as another top-level of the same
    /// application, not as a child of the window it covers — the opposite of
    /// an `AXSheet`. `table::build_table` looks for the modal among the
    /// window's *children*, so the two are handed to it in that shape and
    /// the 250-row budget goes to the dialog, as it does on macOS.
    async fn window_of(&self, app: &AxRef, title: Option<&str>) -> Option<(AxRef, Option<AxRef>)> {
        let frames = self.bus.children(app).await;
        let mut preferred = None;
        let mut first = None;
        let mut dialog = None;
        for frame in frames {
            let Some(role) = self.bus.role(&frame).await else {
                continue;
            };
            let states = self.bus.states(&frame).await;
            match role {
                Role::Dialog | Role::Alert => {
                    if dialog.is_none()
                        && (states.contains(State::Active) || states.contains(State::Modal))
                    {
                        dialog = Some(frame);
                    }
                }
                Role::Frame | Role::Window => {
                    let name = self.bus.name(&frame).await.unwrap_or_default();
                    // The frame the compositor says is active, or the one
                    // whose title matches the window it named, is the one
                    // being looked at. Anything else is a fallback for an
                    // app whose frames carry no state at all.
                    if title.is_some_and(|t| !t.is_empty() && name == t)
                        || states.contains(State::Active)
                    {
                        preferred = Some(frame);
                    } else {
                        first.get_or_insert(frame);
                    }
                }
                _ => {}
            }
        }
        let main = preferred.or(first).or_else(|| dialog.clone())?;
        Some((main, dialog))
    }

    async fn cmd_table(
        &mut self,
        sel: &AppSel,
        goal: Option<&str>,
    ) -> Result<ElementTable, AxError> {
        let app = self.resolve_app(sel)?;
        if AxPolicy::is_chrome_family(&app) {
            return Err(AxError::UseCdp {
                app: app.name.clone(),
            });
        }
        // Chromium and Electron publish nothing while the session says no
        // assistive client is listening. This is the Linux
        // `AXEnhancedUserInterface`: best effort, every observation.
        self.bus.announce().await;

        let title = self.client_for(app.pid).map(|c| c.title);
        let deadline = Instant::now() + TABLE_DEADLINE;
        let window_deadline = Instant::now() + WINDOW_WAIT;
        let (window, dialog) = loop {
            if let Some(root) = self.app_root(app.pid).await
                && let Some(found) = self.window_of(&root, title.as_deref()).await
            {
                break found;
            }
            if Instant::now() >= window_deadline {
                return Err(AxError::NoWindow {
                    app: app.name.clone(),
                });
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
        };

        let mut walker = Walker::new(&self.bus, deadline);
        let walk: Walk = match &dialog {
            Some(sheet) => walker.walk_with_modal(&window, sheet).await,
            None => walker.walk_window(&window).await,
        };
        let menu = walker.walk_menu_bar(&window).await;
        let store = walker.into_store();

        if walk.root.children.is_empty() && menu.is_empty() {
            return Err(AxError::OpaqueApp {
                app: app.name.clone(),
            });
        }

        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);

        let built: BuiltTable = table::build_table(TableInput {
            generation,
            app: app.clone(),
            window: &walk.root,
            menu: &menu,
            goal,
            scroll: table::scroll_affordance(&walk.root),
            settle_timed_out: walk.truncated || Instant::now() >= deadline,
            url: walk.url.clone(),
        });

        let menu_paths = built
            .table
            .elements
            .iter()
            .filter_map(|e| {
                menu.iter()
                    .find(|leaf| {
                        leaf.label() == e.label && e.container.as_deref() == Some("menu bar")
                    })
                    .map(|leaf| (e.index, leaf.path.clone()))
            })
            .collect();

        self.generations.push_back(Generation {
            id: generation,
            pid: app.pid,
            scroll_area: largest_scroll_area(&walk.root),
            store,
            slots: built.raw_ids,
            fingerprints: built.fingerprints,
            elements: built.table.elements.clone(),
            menu_paths,
            window,
        });
        while self.generations.len() > GENERATIONS_KEPT {
            self.generations.pop_front();
        }

        Ok(built.table)
    }

    async fn cmd_guard(&mut self, guard: &Guard) -> Freshness {
        // 5. Session unlocked. Checked first: nothing else matters.
        if self.locked.load(Ordering::SeqCst) {
            return Freshness::Stale(StaleReason::ScreenLocked);
        }

        // 1. Generation current, app still allowed.
        let Some(index) = self
            .generations
            .iter()
            .position(|g| g.id == guard.generation)
        else {
            return Freshness::Stale(StaleReason::Generation);
        };
        if index + 1 != self.generations.len() {
            return Freshness::Stale(StaleReason::Generation);
        }
        let Some(app) = self.app_by_pid(guard.pid) else {
            return Freshness::Stale(StaleReason::ElementGone);
        };
        if self.policy.is_denied(&app) {
            return Freshness::Stale(StaleReason::Denied {
                app: app.bundle_id.unwrap_or(app.name),
            });
        }

        // 3. App active, window unchanged, no dialog that was not there.
        //
        // A run that has the screen lease (A32) owns the keyboard for its
        // duration, so another window having taken focus is not by itself a
        // reason to abandon the step — on an ordinary desktop the window that
        // took it is usually the terminal the run was started from, and every
        // remaining step then fails "app not frontmost" until the budget is
        // gone. Focus is reclaimed once and the question asked again; a
        // window that will not come forward is genuinely stale.
        if crate::seat::may_take_seat() && !self.is_frontmost(guard.pid) {
            self.refocus(guard.pid).await;
            if !self.is_frontmost(guard.pid) {
                let other = self.wm.frontmost().ok().flatten().map_or(-1, |c| c.pid);
                return Freshness::Stale(StaleReason::NotFrontmost { pid: other });
            }
        }
        let Some(root) = self.app_root(guard.pid).await else {
            return Freshness::Stale(StaleReason::WindowChanged);
        };
        let title = self.client_for(guard.pid).map(|c| c.title);
        let Some((window, dialog)) = self.window_of(&root, title.as_deref()).await else {
            return Freshness::Stale(StaleReason::WindowChanged);
        };
        let deadline = Instant::now() + TABLE_DEADLINE;
        let mut walker = Walker::new(&self.bus, deadline);
        let window_node = walker.read_shallow(&window).await;
        if table::window_fingerprint(&window_node) != guard.window {
            return Freshness::Stale(StaleReason::WindowChanged);
        }
        if dialog.is_some() && !guard.modal {
            return Freshness::Stale(StaleReason::SheetAppeared);
        }

        // 2. Element still valid. Actions with no element stop here.
        let Some(target) = guard.target else {
            return Freshness::Fresh;
        };
        let (Some(slot), Some(observed)) = (
            self.slot_of(guard.generation, target.index),
            self.observed_element(guard.generation, target.index)
                .cloned(),
        ) else {
            return Freshness::Stale(StaleReason::Generation);
        };

        let mut live = self.read_slot(guard.generation, slot, deadline).await;
        if live.is_none()
            && self.relocate(guard.generation, target.index).await
            && let Some(slot) = self.slot_of(guard.generation, target.index)
        {
            live = self.read_slot(guard.generation, slot, deadline).await;
        }
        let Some(live) = live else {
            return Freshness::Stale(StaleReason::ElementGone);
        };

        let live_role = crate::mapping::display_role(&live.role, live.subrole.as_deref());
        if live_role != observed.role {
            return Freshness::Stale(StaleReason::RoleChanged {
                was: observed.role.clone(),
                now: live_role,
            });
        }
        let live_label = live.label();
        if !crate::raw::label_matches(&observed.label, &live_label) {
            return Freshness::Stale(StaleReason::LabelChanged {
                was: observed.label.clone(),
                now: live_label,
            });
        }
        if !live.enabled {
            return Freshness::Stale(StaleReason::Disabled);
        }
        // A row the table never measured cannot have moved: a menu leaf is
        // read without opening its menu, so it carries no rect, and
        // comparing a live on-screen rect against that placeholder reports
        // every menu item as moved.
        if !guard.frame.is_unmeasured() && guard.frame.moved_more_than_itself(&live.frame) {
            return Freshness::Stale(StaleReason::Moved);
        }

        // 4. Not occluded, as far as Wayland allows the question to be
        // asked. AT-SPI's hit test is per-object, so it answers "what is on
        // top *inside this window*" and nothing about other applications —
        // and there is no compositor-independent way to ask that. The
        // cross-app case is covered by the active-window check above, which
        // on a Wayland compositor is the same question: an overlapping
        // window that has focus makes this app not frontmost.
        if let Some(reason) = self.occlusion_check(guard, slot, &live.frame).await {
            return Freshness::Stale(reason);
        }

        Freshness::Fresh
    }

    async fn occlusion_check(&self, guard: &Guard, slot: u32, frame: &Rect) -> Option<StaleReason> {
        if !frame.is_visible_size() {
            return None;
        }
        let generation = self.generations.iter().find(|g| g.id == guard.generation)?;
        let target = generation.store.get(slot as usize)?.clone();
        let window = generation.window.clone();
        let (x, y) = frame.center();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a screen coordinate is far inside i32"
        )]
        let hit = self.bus.at_point(&window, x as i32, y as i32).await?;
        if hit == target {
            return None;
        }
        // The same relaxation the macOS backend needed for LibreOffice: an
        // app that draws its own widgets answers the hit test with the
        // canvas or the window, whose frame *encloses* the target. That is
        // not something on top of it.
        //
        // The mirror case is just as common and was refused: a GTK entry
        // answers its own centre with the text node *inside* it, and a
        // rectangle contained by the target is the target's own content —
        // zenity's entry was unfillable because of it.
        let hit_frame = self.bus.extents(&hit).await.unwrap_or_default();
        // A hit with no measurable rectangle has answered nothing, and
        // "nothing" is not "something is on top of it". Under Wayland a
        // toolkit cannot place its widgets on the screen at all: zenity's
        // dialog and the entry inside it both report the origin, and the
        // entry's own centre hit-tests to a label of size 0×0. Reading that
        // as occlusion made every GTK control unpressable.
        if !hit_frame.is_visible_size() {
            return None;
        }
        if hit_frame.contains_rect(frame) || frame.contains_rect(&hit_frame) {
            return None;
        }
        Some(StaleReason::Occluded {
            by: "another element of the same app".to_owned(),
        })
    }

    async fn cmd_act(&mut self, action: &AxAction) -> Result<ActOutcome, AxError> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(AxError::ScreenLocked);
        }
        let pid = self.generations.back().ok_or(AxError::StaleRef)?.pid;
        let app = self.app_by_pid(pid).ok_or(AxError::StaleRef)?;
        if self.policy.is_denied(&app) {
            return Err(AxError::Denied {
                app: app.bundle_id.unwrap_or(app.name),
            });
        }

        match action {
            AxAction::Press { target } => self.do_press(*target).await,
            AxAction::SetValue { target, text } => self.do_set_value(*target, text).await,
            AxAction::SelectOption { target, option } => {
                self.do_select_option(*target, option).await
            }
            AxAction::SelectMenu { path } => self.do_select_menu(path).await,
            AxAction::Key { key, modifiers } => {
                self.require_frontmost(pid)?;
                self.post_key(*key, modifiers)?;
                Ok(ActOutcome {
                    performed: true,
                    method: Method::CgEvent,
                    relocated: false,
                    summary: format!("pressed {key:?}"),
                })
            }
            AxAction::TypeText { text } => {
                self.require_frontmost(pid)?;
                self.post_text(text)?;
                Ok(ActOutcome {
                    performed: true,
                    method: Method::CgEvent,
                    relocated: false,
                    summary: format!("typed {} characters", text.chars().count()),
                })
            }
            AxAction::Scroll { direction, target } => self.do_scroll(*direction, *target).await,
        }
    }

    /// Active is verified live before any synthetic event (01 §Actions).
    fn require_frontmost(&self, pid: i32) -> Result<(), AxError> {
        if !crate::seat::may_take_seat() {
            // Nothing raised the window, so "is it frontmost" is not the
            // question; the question is whether the seat may be used at all.
            return Err(AxError::Unsupported(crate::seat::REFUSED));
        }
        if self.is_frontmost(pid) {
            Ok(())
        } else {
            Err(AxError::Unsupported(
                "the target app is not frontmost; activate it before sending input",
            ))
        }
    }

    /// The virtual keyboard, connected on first use. There is no pointer.
    fn input(&mut self) -> Result<&mut VirtualInput, AxError> {
        // The one gate every synthetic keystroke passes through. Put here
        // rather than at each caller so a path added later cannot forget it:
        // there is no way to reach the virtual keyboard except this function.
        if !crate::seat::may_take_seat() {
            return Err(AxError::Unsupported(crate::seat::REFUSED));
        }
        if self.input.is_none() {
            self.input = Some(VirtualInput::connect(self.wm.name())?);
        }
        self.input.as_mut().ok_or(AxError::NoVirtualInput {
            detail: "the virtual input devices are unavailable".to_owned(),
        })
    }

    fn post_key(&mut self, key: Key, modifiers: &[Modifier]) -> Result<(), AxError> {
        self.input()?.press_key(key, modifiers)
    }

    fn post_text(&mut self, text: &str) -> Result<(), AxError> {
        let kill = Arc::clone(&self.kill);
        let stopped = move || kill.load(Ordering::SeqCst);
        self.input()?.type_text(text, &stopped)
    }

    fn slot_ref(&self, target: Ref) -> Result<AxRef, AxError> {
        let slot = self
            .slot_of(target.generation, target.index)
            .ok_or(AxError::StaleRef)?;
        self.generations
            .iter()
            .find(|g| g.id == target.generation)
            .and_then(|g| g.store.get(slot as usize))
            .cloned()
            .ok_or(AxError::StaleRef)
    }

    async fn do_press(&mut self, target: Ref) -> Result<ActOutcome, AxError> {
        let observed = self
            .observed_element(target.generation, target.index)
            .cloned()
            .ok_or(AxError::StaleRef)?;
        if observed.role == "securefield" {
            return Err(AxError::SecureField);
        }
        // A menu row is an index into the menu bar, not a pressable element.
        if let Some(path) = self.menu_path_of(target) {
            return self.do_select_menu(&path).await;
        }

        let element = self.slot_ref(target)?;
        let names = self.bus.action_names(&element).await;
        if let Some(index) = press_index(&names) {
            let ok = self.bus.do_action(&element, index).await.unwrap_or(false);
            if ok {
                return Ok(ActOutcome {
                    performed: true,
                    method: Method::Ax,
                    relocated: false,
                    summary: format!("pressed {} \"{}\"", observed.role, observed.label),
                });
            }
        }

        // AT-SPI action first; **never the pointer**. A run has to be able to
        // share the machine with the person at it, and a synthetic click is
        // the one thing that cannot be shared: it warps the cursor out from
        // under their hand, and under `follow_mouse` it drags focus with it.
        // The keyboard is seat input too, but it takes only the focused
        // window, which is a thing a person can work around; the pointer is
        // not.
        //
        // So a node that advertises no action is activated the way a keyboard
        // user activates it: focus it, then Space — the toolkit's own
        // activation key for a control, and what WebKitGTK turns into the
        // `click` a web handler listens for. Focus is what makes this legal
        // to do at all: it lands in the element, not at a coordinate.
        let pid = self.generations.back().ok_or(AxError::StaleRef)?.pid;
        if !self.bus.grab_focus(&element).await {
            return Err(AxError::Unsupported(
                "this element advertises no action and will not take focus, so it \
                 cannot be activated without the pointer — which this backend does not use",
            ));
        }
        self.refocus(pid).await;
        self.require_frontmost(pid)?;
        self.post_key(Key::Space, &[])?;
        Ok(ActOutcome {
            performed: true,
            method: Method::CgEvent,
            relocated: false,
            summary: format!("activated {} \"{}\"", observed.role, observed.label),
        })
    }

    async fn do_set_value(&mut self, target: Ref, text: &str) -> Result<ActOutcome, AxError> {
        let observed = self
            .observed_element(target.generation, target.index)
            .cloned()
            .ok_or(AxError::StaleRef)?;
        if observed.role == "securefield" {
            return Err(AxError::SecureField);
        }
        let element = self.slot_ref(target)?;

        // 1. `EditableText.SetTextContents`, the direct write. GTK and
        //    LibreOffice entries take it.
        if self.bus.set_text(&element, text).await
            && let Some(seen) = self.settled_text(&element, text).await
            && seen.trim() == text.trim()
        {
            return Ok(ActOutcome {
                performed: true,
                method: Method::Ax,
                relocated: false,
                summary: format!("set {} \"{}\"", observed.role, observed.label),
            });
        }

        // 2. `Value.SetCurrentValue`, for a spin button or slider.
        if let Ok(number) = text.trim().parse::<f64>()
            && self.bus.set_current_value(&element, number).await
            && self
                .bus
                .current_value(&element)
                .await
                .is_some_and(|v| (v - number).abs() < f64::EPSILON)
        {
            return Ok(ActOutcome {
                performed: true,
                method: Method::Ax,
                relocated: false,
                summary: format!("set {} \"{}\"", observed.role, observed.label),
            });
        }

        // 3. Focus and type. **This is the common path on Linux, not the
        //    exotic one**: WebKitGTK publishes no `EditableText` interface
        //    at all, so every `<input>` in a Tauri window arrives here, as
        //    does every LibreOffice Calc cell.
        self.type_into(&element, text, &observed).await
    }

    async fn type_into(
        &mut self,
        element: &AxRef,
        text: &str,
        observed: &Element,
    ) -> Result<ActOutcome, AxError> {
        let pid = self.generations.back().ok_or(AxError::StaleRef)?.pid;
        self.require_frontmost(pid)?;
        if !self.bus.grab_focus(element).await {
            return Err(AxError::Unsupported(
                "the field neither took the value nor accepted focus",
            ));
        }
        // A field that already holds text would otherwise be appended to,
        // which is the exact defect L1 fixed on the web path. Select-all
        // first — except in a grid, where the parent manages its own
        // descendants and ⌃A would select the whole sheet rather than the
        // cell's contents. Typing into a selected cell replaces it anyway.
        //
        // Nothing is pressed here to "wake" the field. A bare Space sent
        // ahead of the clear is a Space *into the application* whenever the
        // focused node is not really taking text, and an app that binds
        // single keys acts on it: degen-video-studio's console lost focus to
        // the transport that way, and every keystroke after it — the delete,
        // the ⌃A, the value itself — went to the window instead of the
        // field, which then reported the old text and failed the write.
        let in_grid = match self.bus.parent(element).await {
            Some(parent) => self
                .bus
                .states(&parent)
                .await
                .contains(State::ManagesDescendants),
            None => false,
        };
        let existing = self
            .bus
            .text(element)
            .await
            .filter(|text| !text.trim().is_empty());
        // The clear and the value are one operation, not two. Posting them
        // separately meant two keymap uploads inside one write, and keys
        // posted after the second were interpreted against the layout the
        // client still held — the write landed nothing at all.
        let clear = match (existing, in_grid) {
            (Some(existing), false) => existing.chars().count(),
            _ => 0,
        };
        self.replace_text(clear, text)?;
        match self.settled_text(element, text).await {
            Some(seen) if seen.trim() == text.trim() => Ok(ActOutcome {
                performed: true,
                method: Method::CgEvent,
                relocated: false,
                summary: format!("typed into {} \"{}\"", observed.role, observed.label),
            }),
            // An app that shows the text somewhere other than the element's
            // own `Text` is common (a spreadsheet cell mid-edit). This actor
            // will not call an unverifiable write a success.
            _ => Err(AxError::Unsupported(
                "typed into the field but the value did not appear; the app may need a commit key",
            )),
        }
    }

    /// Clear whatever the focused field holds and type `text` into it, in one
    /// uploaded keymap.
    fn replace_text(&mut self, clear: usize, text: &str) -> Result<(), AxError> {
        let kill = Arc::clone(&self.kill);
        let stopped = move || kill.load(Ordering::SeqCst);
        self.input()?.replace_text(clear, text, &stopped)
    }

    async fn settled_text(&self, element: &AxRef, wanted: &str) -> Option<String> {
        let deadline = Instant::now() + TYPED_VALUE_WAIT;
        loop {
            let seen = self.bus.text(element).await;
            if seen
                .as_deref()
                .is_some_and(|value| value.trim() == wanted.trim())
                || Instant::now() >= deadline
            {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    async fn do_select_option(&mut self, target: Ref, option: &str) -> Result<ActOutcome, AxError> {
        let element = self.slot_ref(target)?;
        for child in self.bus.children(&element).await {
            let name = self.bus.name(&child).await.unwrap_or_default();
            if name.trim() != option.trim() {
                continue;
            }
            let names = self.bus.action_names(&child).await;
            if let Some(index) = press_index(&names)
                && self.bus.do_action(&child, index).await.unwrap_or(false)
            {
                return Ok(ActOutcome {
                    performed: true,
                    method: Method::Ax,
                    relocated: false,
                    summary: format!("selected \"{option}\""),
                });
            }
        }
        // A combo box with a text entry takes the string directly. Tried in
        // two ways, because the ARIA shape publishes no `EditableText`: the
        // interface first, then the keyboard — which is how a person chooses
        // from such a list and what the control's own handler listens for.
        if self.bus.set_text(&element, option).await {
            return Ok(ActOutcome {
                performed: true,
                method: Method::Ax,
                relocated: false,
                summary: format!("selected \"{option}\""),
            });
        }
        // Nothing about this control takes a value, so it is driven the way a
        // person drives it: move the highlight onto the wanted row and commit.
        //
        // Not by typing the row's text. A list like this labels its rows
        // `<id> — <description>`, and typing that whole label back into the
        // box filters against the description too and can match nothing at
        // all; the box is then cleared by the app's own choose handler and
        // the commit lands on an empty list. The offsets below come from the
        // options the observation already recorded, in the order the list
        // published them, so no re-reading is needed between keystrokes.
        let offset = self
            .observed_element(target.generation, target.index)
            .and_then(|observed| {
                observed
                    .options
                    .iter()
                    .position(|candidate| candidate.trim() == option.trim())
            })
            .ok_or(AxError::Unsupported(
                "that option is not one this control offered",
            ))?;
        if self.bus.grab_focus(&element).await {
            // The first row is highlighted as soon as the list opens, so the
            // wanted one is `offset` presses down from it.
            for _ in 0..offset {
                self.post_key(Key::Down, &[])?;
            }
            self.post_key(Key::Return, &[])?;
            return Ok(ActOutcome {
                performed: true,
                method: Method::CgEvent,
                relocated: false,
                summary: format!("selected \"{option}\""),
            });
        }
        Err(AxError::Unsupported(
            "that option is not among the element's children and the control takes no value",
        ))
    }

    async fn do_select_menu(&mut self, path: &[String]) -> Result<ActOutcome, AxError> {
        if path.is_empty() {
            return Err(AxError::Unsupported("an empty menu path selects nothing"));
        }
        let pid = self.generations.back().ok_or(AxError::StaleRef)?.pid;
        let root = self.app_root(pid).await.ok_or(AxError::StaleRef)?;
        let title = self.client_for(pid).map(|c| c.title);
        let (window, _) =
            self.window_of(&root, title.as_deref())
                .await
                .ok_or(AxError::NoWindow {
                    app: pid.to_string(),
                })?;
        let deadline = Instant::now() + TABLE_DEADLINE;
        let mut walker = Walker::new(&self.bus, deadline);
        let leaves = walker.walk_menu_bar(&window).await;
        let store = walker.into_store();
        let leaf = leaves
            .iter()
            .find(|leaf| leaf.path == path)
            .ok_or(AxError::Unsupported("no menu item has that path"))?;
        if !leaf.enabled {
            return Err(AxError::Unsupported("that menu item is disabled"));
        }
        let element = store
            .get(leaf.id as usize)
            .cloned()
            .ok_or(AxError::StaleRef)?;
        let names = self.bus.action_names(&element).await;
        let index = press_index(&names)
            .ok_or(AxError::Unsupported("that menu item advertises no action"))?;
        if !self.bus.do_action(&element, index).await.unwrap_or(false) {
            return Err(AxError::Unsupported("the menu item refused to activate"));
        }
        Ok(ActOutcome {
            performed: true,
            method: Method::Ax,
            relocated: false,
            summary: format!("chose menu {}", path.join(" › ")),
        })
    }

    async fn do_scroll(
        &mut self,
        direction: ScrollDir,
        target: Option<Ref>,
    ) -> Result<ActOutcome, AxError> {
        // AT-SPI first: scrolling an element into view needs no synthetic
        // event.
        if let Some(target) = target
            && let Ok(element) = self.slot_ref(target)
            && self.bus.scroll_into_view(&element).await
        {
            return Ok(ActOutcome {
                performed: true,
                method: Method::Ax,
                relocated: false,
                summary: "scrolled the element into view".to_owned(),
            });
        }

        // The wheel is the pointer, so it is not used. A page-sized scroll is
        // what the arrow keys give the focused scroll area, and it reaches
        // the same view without the cursor leaving wherever the person left
        // it. Fewer pixels per step than a wheel notch, so the count is the
        // one the old notch maths produced, spent on key presses instead.
        let pid = self.generations.back().ok_or(AxError::StaleRef)?.pid;
        self.refocus(pid).await;
        self.require_frontmost(pid)?;
        let area = self
            .generations
            .back()
            .and_then(|g| g.scroll_area)
            .unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                w: 800.0,
                h: 600.0,
            });
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a step count derived from a window height is far inside i32"
        )]
        let steps = ((area.h * 0.8) / 60.0).clamp(1.0, 20.0) as i32;
        let key = match direction {
            ScrollDir::Up => Key::Up,
            ScrollDir::Down => Key::Down,
        };
        for _ in 0..steps {
            self.post_key(key, &[])?;
        }
        Ok(ActOutcome {
            performed: true,
            method: Method::CgEvent,
            relocated: false,
            summary: format!("scrolled {direction:?} by {steps} steps"),
        })
    }

    // -- generation bookkeeping -------------------------------------------

    fn slot_of(&self, generation: u32, index: u16) -> Option<u32> {
        let g = self.generations.iter().find(|g| g.id == generation)?;
        g.slots.get(index as usize).copied()
    }

    fn menu_path_of(&self, target: Ref) -> Option<Vec<String>> {
        let g = self
            .generations
            .iter()
            .find(|g| g.id == target.generation)?;
        g.menu_paths
            .iter()
            .find(|(index, _)| *index == target.index)
            .map(|(_, path)| path.clone())
    }

    fn observed_element(&self, generation: u32, index: u16) -> Option<&Element> {
        let g = self.generations.iter().find(|g| g.id == generation)?;
        g.elements.get(index as usize)
    }

    async fn read_slot(&self, generation: u32, slot: u32, deadline: Instant) -> Option<RawNode> {
        let g = self.generations.iter().find(|g| g.id == generation)?;
        let element = g.store.get(slot as usize)?.clone();
        let mut walker = Walker::new(&self.bus, deadline);
        let node = walker.read_shallow(&element).await;
        // An object whose role no longer reads at all is dead.
        (node.role != "AXUnknown").then_some(node)
    }

    /// Relocate a dead element by fingerprint, exactly once.
    async fn relocate(&mut self, generation: u32, index: u16) -> bool {
        let Some(g) = self.generations.iter().find(|g| g.id == generation) else {
            return false;
        };
        let Some(wanted) = g.fingerprints.get(index as usize).cloned() else {
            return false;
        };
        let window = g.window.clone();
        let deadline = Instant::now() + TABLE_DEADLINE;
        let mut walker = Walker::new(&self.bus, deadline);
        let walk = walker.walk_window(&window).await;
        let store = walker.into_store();

        let mut matches: Vec<u32> = Vec::new();
        walk.root.walk(&mut |node| {
            if node.role == wanted.role
                && node.subrole == wanted.subrole
                && node.label() == wanted.label
                && node.identifier == wanted.identifier
            {
                matches.push(node.id);
            }
        });
        if matches.len() != 1 {
            return false;
        }
        let Some(found) = matches
            .first()
            .and_then(|id| store.get(*id as usize))
            .cloned()
        else {
            return false;
        };
        let Some(g) = self.generations.iter_mut().find(|g| g.id == generation) else {
            return false;
        };
        let Some(slot) = g.slots.get(index as usize).copied() else {
            return false;
        };
        let Some(entry) = g.store.get_mut(slot as usize) else {
            return false;
        };
        *entry = found;
        true
    }
}

/// Which advertised action to invoke, if any.
fn press_index(names: &[String]) -> Option<i32> {
    if names.is_empty() {
        return None;
    }
    for verb in PRESS_VERBS {
        if let Some(index) = names.iter().position(|n| n.eq_ignore_ascii_case(verb)) {
            return i32::try_from(index).ok();
        }
    }
    // An unrecognised verb is still an action. Chromium answers `GetName`
    // with `click`, LibreOffice with `press`, and a GTK4 cell renderer with
    // something else entirely.
    Some(0)
}

/// Fold a caught panic into the error the caller sees.
fn unwind<T>(
    command: &'static str,
    out: Result<Result<T, AxError>, Box<dyn std::any::Any + Send>>,
) -> Result<T, AxError> {
    match out {
        Ok(value) => value,
        Err(_) => Err(AxError::Panic { command }),
    }
}

/// The frame of the largest scroll area in the walked tree.
fn largest_scroll_area(root: &RawNode) -> Option<Rect> {
    let mut best: Option<Rect> = None;
    root.walk(&mut |node| {
        if node.role != "AXScrollArea" {
            return;
        }
        let area = node.frame.w * node.frame.h;
        if best.is_none_or(|b| area > b.w * b.h) {
            best = Some(node.frame);
        }
    });
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handle must be usable from tokio and from several tasks at once.
    #[test]
    fn handle_is_send_sync_clone_and_static() {
        fn assert_send<T: Send + Sync + Clone + 'static>() {}
        assert_send::<AxHandle>();
    }

    /// Every toolkit on this machine names the press action differently:
    /// LibreOffice and WebKitGTK say `press`, Chromium says `click`. An
    /// unrecognised verb is still the only action the object has, so it is
    /// invoked rather than refused — refusing is how a button that works
    /// reads as broken.
    #[test]
    fn press_picks_a_known_verb_then_falls_back_to_the_first() {
        assert_eq!(press_index(&["press".to_owned()]), Some(0));
        assert_eq!(
            press_index(&["showmenu".to_owned(), "click".to_owned()]),
            Some(1)
        );
        assert_eq!(press_index(&["do the thing".to_owned()]), Some(0));
        assert_eq!(press_index(&[]), None);
    }
}
