//! The AX actor: one OS thread, one `CFRunLoop`, one command at a time.
//!
//! `AXUIElement` is not `Send`, so every AX call happens here and nothing but
//! plain data crosses the channel. The thread alternates between 10 ms run
//! loop slices (needed for observer sources and for CF's own plumbing) and
//! draining the command channel, so no command waits more than one slice.
//!
//! Three safety properties are structural rather than hoped for:
//!
//! * Every command body runs inside `catch_unwind`. A panic fails that one
//!   command, bumps the generation so every outstanding ref dies, and the
//!   actor carries on.
//! * Every cluster of AX calls runs inside `objc2::exception::catch`, so a
//!   misbehaving app raising an Objective-C exception cannot unwind across
//!   FFI and abort the process.
//! * The kill switch is an `AtomicBool` read between run loop slices and
//!   between typed chunks, never a message, so it is honoured within ~10 ms
//!   even while a long string is being typed.

#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use tokio::sync::oneshot;

use super::apps;
use super::input::{self, Target};
use super::perm;
use super::sys::{self, Attrs, AxElem};
use crate::deny::AxPolicy;
use crate::error::{AxError, Freshness, StaleReason};
use crate::mapping::PRESS_ACTIONS;
use crate::raw::RawNode;

use crate::table::{self, BuiltTable, TableInput};
use crate::types::{
    ActOutcome, AppInfo, AppSel, AxAction, Element, ElementTable, Fingerprint, Guard, Method, Rect,
    Ref,
};

/// One run loop slice. A command never waits longer than this.
const SLICE: f64 = 0.010;
/// Wall deadline for one element table (01 §Messaging timeouts).
const TABLE_DEADLINE: Duration = Duration::from_millis(400);
/// How long an app that just came forward is given to publish a window.
const WINDOW_WAIT: Duration = Duration::from_millis(1_500);
/// How long an app is given to take the menu bar after it is asked to. A cold
/// launch spends most of it: the process exists within a fifth of a second,
/// but refuses activation until its own run loop is up.
const ACTIVATE_DEADLINE: Duration = Duration::from_millis(3_000);
/// How long a typed value is given to appear in the field's `AXValue`.
///
/// A synthesized keystroke is delivered to the app asynchronously, and a
/// WebKit-backed field — Neo's own desktop composer is one — republishes its
/// value a frame or two after the last key lands. Reading once, immediately,
/// called those writes failures ("typed into the field but the value did not
/// appear") on text that was demonstrably in the field a moment later.
const TYPED_VALUE_WAIT: Duration = Duration::from_millis(600);
/// How many generations of refs stay alive.
const GENERATIONS_KEPT: usize = 2;
/// How far up the parent chain a hit test may answer with an ancestor.
const OCCLUSION_HOPS: usize = 8;

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
    ///
    /// Every command carries a reply channel, and a caller awaiting one must
    /// be told rather than left waiting: dropping the sender would surface as
    /// [`AxError::ActorDead`], which is a different thing to report.
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

/// A `Clone + Send` handle onto the AX thread.
///
/// Cloning is cheap and every clone talks to the same actor. Dropping the
/// last clone shuts the thread down.
#[derive(Clone)]
pub struct AxHandle {
    tx: mpsc::Sender<AxCmd>,
    kill: Arc<AtomicBool>,
    locked: Arc<AtomicBool>,
}

impl AxHandle {
    /// Start the actor thread.
    ///
    /// Succeeds without the Accessibility grant: [`AxHandle::apps`] and
    /// [`AxHandle::activate`] need no permission, and the commands that do
    /// fail with [`AxError::NotTrusted`] until the grant lands.
    ///
    /// # Errors
    ///
    /// Fails when the OS refuses to start the thread.
    pub fn spawn() -> Result<Self, AxError> {
        Self::spawn_with_policy(AxPolicy::new(std::process::id().cast_signed()))
    }

    /// Start the actor thread with a tightened deny list.
    ///
    /// # Errors
    ///
    /// Fails when the OS refuses to start the thread.
    pub fn spawn_with_policy(policy: AxPolicy) -> Result<Self, AxError> {
        let (tx, rx) = mpsc::channel::<AxCmd>();
        let kill = Arc::new(AtomicBool::new(false));
        let locked = Arc::new(AtomicBool::new(false));
        let thread_kill = Arc::clone(&kill);
        let thread_locked = Arc::clone(&locked);

        std::thread::Builder::new()
            .name("neo-ax".to_owned())
            .spawn(move || {
                let mut actor = Actor::new(policy, thread_kill, thread_locked);
                actor.run(&rx);
            })
            .map_err(|_| AxError::ActorDead)?;

        Ok(Self { tx, kill, locked })
    }

    /// Whether this binary is a trusted accessibility client. Never prompts.
    ///
    /// The grant attaches to the **binary that runs**, so under `cargo run`
    /// it is the launching terminal that must be granted, not Neo. See
    /// [`crate::perm`] for the whole story.
    #[must_use]
    pub fn trusted() -> bool {
        perm::trusted()
    }

    /// Whether this binary is trusted, showing the system prompt if not.
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
    /// Lock-free and non-blocking, because the caller is a stop button and
    /// the actor may be mid-command: the flag is read between run loop
    /// slices and between typed chunks, so a stop lands within ~10 ms even
    /// while a long string is being typed. Every modifier comes back up
    /// once, and every later command fails with [`AxError::Stopped`] rather
    /// than half-driving an application.
    ///
    /// Sticky on purpose, and there is no `resume`: the handle is cheap, and
    /// a run that was stopped should start a fresh one rather than inherit a
    /// half-typed field and a generation of refs taken before the user
    /// intervened.
    ///
    /// **What this cannot reclaim.** Keystrokes and clicks already delivered
    /// are gone into the application; nothing here can un-type them, and a
    /// document the app has already saved stays saved. It stops what comes
    /// next.
    pub fn stop(&self) {
        self.kill.store(true, Ordering::SeqCst);
    }

    /// Whether [`AxHandle::stop`] has been called. A caller bridging another
    /// cancellation source reads this rather than tracking it separately.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.kill.load(Ordering::SeqCst)
    }

    /// Every regular running application.
    ///
    /// # Errors
    ///
    /// Fails when the actor thread is gone.
    pub async fn apps(&self) -> Result<Vec<AppInfo>, AxError> {
        self.call(AxCmd::Apps).await
    }

    /// Bring an app to the front and wait for it to get there.
    ///
    /// # Errors
    ///
    /// Fails when no app matches, when the app is denied, or when it does not
    /// come forward.
    pub async fn activate(&self, app: &AppSel) -> Result<AppInfo, AxError> {
        let app = app.clone();
        self.call(|reply| AxCmd::Activate { app, reply }).await
    }

    /// One observation of an app: the element table of 01 §Element table.
    ///
    /// # Errors
    ///
    /// Fails without the Accessibility grant, on a denied app, on a
    /// Chrome-family browser (the web path owns those), and when the app has
    /// no focused window.
    pub async fn table(&self, app: &AppSel) -> Result<ElementTable, AxError> {
        self.table_for_goal(app, None).await
    }

    /// Like [`AxHandle::table`], ranking menu leaves by overlap with the goal
    /// so the ones worth offering survive the 250-row budget.
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

    /// Execute one chosen action, AX first.
    ///
    /// # Errors
    ///
    /// Fails on a stale ref, a denied app, secure input, a locked screen, and
    /// when the element offers no way to perform the action.
    pub async fn act(&self, action: &AxAction) -> Result<ActOutcome, AxError> {
        let action = Box::new(action.clone());
        self.call(|reply| AxCmd::Act { action, reply }).await
    }

    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> AxCmd) -> Result<T, AxError> {
        // Checked here as well as on the actor thread: a stop must be honoured
        // even while the actor is still inside the command that was running
        // when it landed.
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
        // Only the last clone's send arrives at a live receiver; a failure
        // here just means the actor is already gone.
        let _ = self.tx.send(AxCmd::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

/// One observation's worth of live elements, kept on the actor thread.
struct Generation {
    id: u32,
    pid: i32,
    /// Walk-order element store; `RawNode::id` indexes it.
    store: Vec<AxElem>,
    /// Element index -> slot in `store`.
    slots: Vec<u32>,
    fingerprints: Vec<Fingerprint>,
    elements: Vec<Element>,
    /// Frame of the largest scroll area, for the scroll fallback.
    scroll_area: Option<Rect>,
    /// Menu paths by element index, for `MENU` rows.
    menu_paths: Vec<(u16, Vec<String>)>,
}

struct Actor {
    attrs: Attrs,
    system: AxElem,
    policy: AxPolicy,
    next_generation: u32,
    generations: VecDeque<Generation>,
    kill: Arc<AtomicBool>,
    locked: Arc<AtomicBool>,
}

impl Actor {
    fn new(policy: AxPolicy, kill: Arc<AtomicBool>, locked: Arc<AtomicBool>) -> Self {
        let system = AxElem::system_wide();
        // Process-wide default: the OS default is far longer and one hung app
        // would freeze the actor.
        system.set_messaging_timeout(sys::GLOBAL_MESSAGING_TIMEOUT);
        Self {
            attrs: Attrs::new(),
            system,
            policy,
            next_generation: 1,
            generations: VecDeque::new(),
            kill,
            locked,
        }
    }

    fn run(&mut self, rx: &mpsc::Receiver<AxCmd>) {
        // Whether the stop has already been acted on. The flag is sticky, so
        // without this the modifiers would be released again on every slice
        // for the rest of the actor's life.
        let mut stopped = false;
        loop {
            // SAFETY: `kCFRunLoopDefaultMode` is a CoreFoundation string
            // constant valid for the process lifetime. Reading it is the only
            // unsafe part; running the loop on the thread that owns it is
            // exactly what CFRunLoop is for.
            let mode = unsafe { kCFRunLoopDefaultMode };
            CFRunLoop::run_in_mode(mode, SLICE, true);

            if !stopped && self.kill.load(Ordering::SeqCst) {
                input::release_all_modifiers();
                stopped = true;
            }

            loop {
                match rx.try_recv() {
                    Ok(AxCmd::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return,
                    // A stopped actor answers, but does nothing: a caller
                    // awaiting a reply is told it was stopped rather than
                    // left to time out.
                    Ok(cmd) if stopped => cmd.refuse(),
                    Ok(cmd) => self.dispatch(cmd),
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
        }
    }

    fn dispatch(&mut self, cmd: AxCmd) {
        let name = cmd.name();
        match cmd {
            AxCmd::Apps(reply) => {
                let out = self.protected(name, |a| Ok(a.cmd_apps()));
                let _ = reply.send(out);
            }
            AxCmd::Activate { app, reply } => {
                let out = self.protected(name, |a| a.cmd_activate(&app));
                let _ = reply.send(out);
            }
            AxCmd::Table { app, goal, reply } => {
                let out = self.protected(name, |a| a.cmd_table(&app, goal.as_deref()));
                let _ = reply.send(out);
            }
            AxCmd::Guard { guard, reply } => {
                let out = self.protected(name, |a| Ok(a.cmd_guard(&guard)));
                let _ = reply.send(out);
            }
            AxCmd::Act { action, reply } => {
                let out = self.protected(name, |a| a.cmd_act(&action));
                let _ = reply.send(out);
            }
            AxCmd::Shutdown => {}
        }
    }

    /// Run one command body under `catch_unwind`.
    ///
    /// A panic fails that command and bumps the generation, so no caller can
    /// act on a ref minted before the actor's state became suspect.
    fn protected<T>(
        &mut self,
        name: &'static str,
        body: impl FnOnce(&mut Self) -> Result<T, AxError>,
    ) -> Result<T, AxError> {
        match std::panic::catch_unwind(AssertUnwindSafe(|| body(self))) {
            Ok(out) => out,
            Err(_) => {
                self.invalidate_all();
                Err(AxError::Panic { command: name })
            }
        }
    }

    /// Every outstanding ref dies.
    fn invalidate_all(&mut self) {
        self.generations.clear();
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
    }

    // -- commands ----------------------------------------------------------

    fn cmd_apps(&self) -> Vec<AppInfo> {
        apps::running()
            .into_iter()
            .filter(|a| !self.policy.is_denied(a))
            .collect()
    }

    fn resolve_app(&self, sel: &AppSel) -> Result<AppInfo, AxError> {
        let all = apps::running();
        let found = sel.pick(&all).cloned().ok_or_else(|| AxError::NoApp {
            selector: sel.to_string(),
        })?;
        if self.policy.is_denied(&found) {
            return Err(AxError::Denied {
                app: found.bundle_id.clone().unwrap_or(found.name.clone()),
            });
        }
        Ok(found)
    }

    /// Poll until a just-launched app shows up in the process list.
    fn wait_for_app(&self, sel: &AppSel) -> Result<AppInfo, AxError> {
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline {
            if let Ok(app) = self.resolve_app(sel) {
                return Ok(app);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(AxError::NoApp {
            selector: sel.to_string(),
        })
    }

    fn cmd_activate(&mut self, sel: &AppSel) -> Result<AppInfo, AxError> {
        let app = match self.resolve_app(sel) {
            Ok(app) => app,
            // "Activate first" is the first input rule, and an app that is
            // not running yet cannot be activated. A name or a bundle id can
            // still be launched — and the bundle id has to be, because it is
            // the selector the navigator prefers — but only after the deny
            // list has cleared it. A pid that is gone stays gone.
            Err(AxError::NoApp { .. }) => {
                let probe = match sel {
                    AppSel::Name(name) => AppInfo {
                        name: name.clone(),
                        bundle_id: None,
                        pid: -1,
                        frontmost: false,
                    },
                    AppSel::BundleId(bundle_id) => AppInfo {
                        name: String::new(),
                        bundle_id: Some(bundle_id.clone()),
                        pid: -1,
                        frontmost: false,
                    },
                    // Nothing to launch: a dead pid does not come back, and
                    // "whatever is frontmost" named no app in the first place.
                    AppSel::Pid(_) | AppSel::Frontmost => {
                        return Err(AxError::NoApp {
                            selector: sel.to_string(),
                        });
                    }
                };
                if self.policy.is_denied(&probe) {
                    return Err(AxError::Denied {
                        app: sel.to_string(),
                    });
                }
                // The launch boolean is advisory: LaunchServices answers NO
                // while a previous instance is still shutting down, yet the
                // app comes up a moment later. What matters is whether it
                // shows up at all.
                let accepted = match sel {
                    AppSel::Name(name) => apps::launch(name),
                    AppSel::BundleId(bundle_id) => apps::launch_bundle_id(bundle_id),
                    AppSel::Pid(_) | AppSel::Frontmost => false,
                };
                match self.wait_for_app(sel) {
                    Ok(app) => app,
                    Err(_) if !accepted => {
                        return Err(AxError::NoApp {
                            selector: sel.to_string(),
                        });
                    }
                    Err(other) => return Err(other),
                }
            }
            Err(other) => return Err(other),
        };
        // An app that restarted has a new pid: nothing observed before it
        // came back may be acted on.
        self.generations
            .retain(|g| g.pid != app.pid || apps::by_pid(g.pid).is_some());
        // Activation is asynchronous, and an app that is still coming up
        // refuses it outright: a Numbers launched a moment earlier answered NO
        // to `activateWithOptions` and then came forward by itself, which used
        // to fail the whole run with "no running application matches". So the
        // request is repeated until the menu bar changes hands, and only an
        // app that never accepted one is reported as unreachable.
        let deadline = Instant::now() + ACTIVATE_DEADLINE;
        let mut accepted = false;
        loop {
            accepted |= apps::activate(app.pid);
            if apps::is_frontmost(app.pid)
                && let Some(front) = apps::by_pid(app.pid)
            {
                return Ok(front);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !accepted {
            return Err(AxError::NoApp {
                selector: sel.to_string(),
            });
        }
        apps::by_pid(app.pid).ok_or(AxError::NoApp {
            selector: sel.to_string(),
        })
    }

    /// Ask an app to publish the tree it keeps for assistive clients.
    ///
    /// Some apps render their own content and expose almost nothing until
    /// asked. Measured: **Numbers** offered exactly one element — the sheet
    /// title field — with its entire grid absent, while LibreOffice Calc
    /// offered 92 addressable cells. Two switches fix that class of app:
    /// `AXEnhancedUserInterface`, which AppKit apps honour (it is what
    /// VoiceOver sets), and `AXManualAccessibility`, which Electron apps
    /// require (A19 names it for Powermove).
    ///
    /// Setting both is safe on an app that wants neither: the write is simply
    /// refused, which is why the result is discarded. It is done on every
    /// observation rather than once, because an app that restarts forgets.
    fn publish_full_tree(&self, app_elem: &AxElem, app: &str) {
        let yes = sys::cf_true();
        let _ = sys::guarded(app, "set_enhanced_ui", || {
            app_elem.set_attr(&self.attrs.enhanced_ui, yes)
        });
        let _ = sys::guarded(app, "set_manual_accessibility", || {
            app_elem.set_attr(&self.attrs.manual_accessibility, yes)
        });
    }

    /// The window to read: the focused one, then the main one, then the first
    /// the app lists.
    ///
    /// An app started outside the Dock — a `cargo tauri dev` build, a helper
    /// launched from a terminal — keeps a window that is on screen and never
    /// becomes key, so `AXFocusedWindow` is empty for it and reading only
    /// that attribute made the app unobservable ("has no focused window")
    /// even though its tree reads perfectly through `AXMainWindow`.
    fn window_of(&self, app_elem: &AxElem, app: &str) -> Result<Option<AxElem>, AxError> {
        for (attribute, call) in [
            (&self.attrs.focused_window, "focused_window"),
            (&self.attrs.main_window, "main_window"),
            (&self.attrs.windows, "windows"),
        ] {
            let found = sys::guarded(app, call, || app_elem.attr(attribute))?
                .and_then(|value| sys::first_element(&value));
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    fn cmd_table(&mut self, sel: &AppSel, goal: Option<&str>) -> Result<ElementTable, AxError> {
        if !perm::trusted() {
            return Err(AxError::NotTrusted);
        }
        let app = self.resolve_app(sel)?;
        if AxPolicy::is_chrome_family(&app) {
            return Err(AxError::UseCdp {
                app: app.name.clone(),
            });
        }

        let app_elem = AxElem::app(app.pid);
        app_elem.set_messaging_timeout(sys::APP_MESSAGING_TIMEOUT);
        self.publish_full_tree(&app_elem, &app.name);
        let deadline = Instant::now() + TABLE_DEADLINE;

        // An app that has just come forward may not have published its
        // focused window yet, so give it a moment before giving up. This is
        // the normal shape of an "activate, then observe" step.
        let window_elem = {
            let window_deadline = Instant::now() + WINDOW_WAIT;
            loop {
                let found = self.window_of(&app_elem, &app.name)?;
                match found {
                    Some(elem) => break elem,
                    None if Instant::now() < window_deadline => {
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    None => {
                        return Err(AxError::NoWindow {
                            app: app.name.clone(),
                        });
                    }
                }
            }
        };

        let mut walk = sys::walk(&window_elem, &self.attrs, &app.name, deadline);
        let menu = sys::walk_menu_bar(&app_elem, &self.attrs, &app.name, &mut walk.store, deadline);

        if walk.root.children.is_empty() && walk.root.label().is_empty() {
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
            settle_timed_out: Instant::now() >= deadline,
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
            store: walk.store,
            slots: built.raw_ids,
            fingerprints: built.fingerprints,
            elements: built.table.elements.clone(),
            scroll_area: largest_scroll_area(&walk.root),
            menu_paths,
        });
        while self.generations.len() > GENERATIONS_KEPT {
            self.generations.pop_front();
        }

        Ok(built.table)
    }

    fn cmd_guard(&mut self, guard: &Guard) -> Freshness {
        // 5. Session unlocked. Checked first: nothing else matters.
        if self.locked.load(Ordering::SeqCst) {
            return Freshness::Stale(StaleReason::ScreenLocked);
        }

        // 1. Generation current, app still allowed.
        let Some(gen_index) = self
            .generations
            .iter()
            .position(|g| g.id == guard.generation)
        else {
            return Freshness::Stale(StaleReason::Generation);
        };
        if gen_index + 1 != self.generations.len() {
            return Freshness::Stale(StaleReason::Generation);
        }
        let Some(app) = apps::by_pid(guard.pid) else {
            return Freshness::Stale(StaleReason::ElementGone);
        };
        if self.policy.is_denied(&app) {
            return Freshness::Stale(StaleReason::Denied {
                app: app.bundle_id.unwrap_or(app.name),
            });
        }

        // 3. App frontmost, window unchanged, no new sheet.
        if !apps::is_frontmost(guard.pid) {
            let other = apps::frontmost().map_or(-1, |a| a.pid);
            return Freshness::Stale(StaleReason::NotFrontmost { pid: other });
        }
        let app_elem = AxElem::app(guard.pid);
        app_elem.set_messaging_timeout(sys::APP_MESSAGING_TIMEOUT);
        let Ok(Some(window_value)) = sys::guarded(&app.name, "focused_window", || {
            app_elem.attr(&self.attrs.focused_window)
        }) else {
            return Freshness::Stale(StaleReason::WindowChanged);
        };
        let Some(window_elem) = sys::first_element(&window_value) else {
            return Freshness::Stale(StaleReason::WindowChanged);
        };
        let window_node = sys::read_shallow(&window_elem, &self.attrs, &app.name);
        if table::window_fingerprint(&window_node) != guard.window {
            return Freshness::Stale(StaleReason::WindowChanged);
        }
        let modal_now = sys::has_modal_child(&window_elem, &self.attrs, &app.name);
        if modal_now && !guard.modal {
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

        let mut live = self.read_slot(guard.generation, slot, &app.name);
        if live.is_none() {
            // Dead: relocate once by fingerprint.
            if self.relocate(guard.generation, target.index, &app.name) {
                live = self
                    .slot_of(guard.generation, target.index)
                    .and_then(|s| self.read_slot(guard.generation, s, &app.name));
            }
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

        // 4. Not occluded.
        if let Some(reason) = self.occlusion_check(guard, slot, &live.frame) {
            return Freshness::Stale(reason);
        }

        Freshness::Fresh
    }

    fn occlusion_check(&self, guard: &Guard, slot: u32, frame: &Rect) -> Option<StaleReason> {
        if !frame.is_visible_size() {
            return None;
        }
        let (x, y) = frame.center();
        let hit = self.system.element_at(x, y)?;
        let hit_pid = hit.pid()?;
        if hit_pid != guard.pid {
            let name = apps::by_pid(hit_pid).map_or_else(|| hit_pid.to_string(), |a| a.name);
            return Some(StaleReason::Occluded { by: name });
        }
        let generation = self.generations.iter().find(|g| g.id == guard.generation)?;
        let target = generation.store.get(slot as usize)?;
        if sys::is_same_or_related(&hit, target, &self.attrs, OCCLUSION_HOPS) {
            return None;
        }
        // Deviation from 01 §Guards check 4, found driving LibreOffice: an app
        // that draws its own widgets (LibreOffice, Electron, a Java toolkit)
        // answers the hit-test with a synthetic parent — a canvas or the
        // window — whose `AXParent` chain does not reach the target within the
        // hop budget. Every button in such an app would be permanently
        // "occluded by another element of the same app" and nothing would ever
        // execute. A same-app element whose frame *encloses* the target is not
        // something on top of it, so it is accepted; anything else still is.
        let hit_frame = sys::read_shallow(&hit, &self.attrs, "hit-test").frame;
        if hit_frame.is_visible_size() && hit_frame.contains_rect(frame) {
            return None;
        }
        Some(StaleReason::Occluded {
            by: "another element of the same app".to_owned(),
        })
    }

    fn cmd_act(&mut self, action: &AxAction) -> Result<ActOutcome, AxError> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(AxError::ScreenLocked);
        }
        if !perm::trusted() {
            return Err(AxError::NotTrusted);
        }
        let generation = self.generations.back().ok_or(AxError::StaleRef)?;
        let pid = generation.pid;
        let app = apps::by_pid(pid).ok_or(AxError::StaleRef)?;
        if self.policy.is_denied(&app) {
            return Err(AxError::Denied {
                app: app.bundle_id.unwrap_or(app.name),
            });
        }

        match action {
            AxAction::Press { target } => self.do_press(*target, &app.name),
            AxAction::SetValue { target, text } => self.do_set_value(*target, text, &app.name),
            AxAction::SelectOption { target, option } => {
                self.do_select_option(*target, option, &app.name)
            }
            AxAction::SelectMenu { path } => self.do_select_menu(pid, path, &app.name),
            AxAction::Key { key, modifiers } => {
                self.require_frontmost(pid)?;
                input::press_key(*key, modifiers, Target::Pid(pid))?;
                Ok(ActOutcome {
                    performed: true,
                    method: Method::CgEvent,
                    relocated: false,
                    summary: format!("pressed {key:?}"),
                })
            }
            AxAction::TypeText { text } => {
                self.require_frontmost(pid)?;
                input::type_text(text, Target::Pid(pid), &self.kill)?;
                Ok(ActOutcome {
                    performed: true,
                    method: Method::CgEvent,
                    relocated: false,
                    summary: format!("typed {} characters", text.chars().count()),
                })
            }
            AxAction::Scroll { direction, target } => {
                self.do_scroll(*direction, *target, pid, &app.name)
            }
        }
    }

    /// Frontmost is verified live before any HID-level event (01 §Actions).
    fn require_frontmost(&self, pid: i32) -> Result<(), AxError> {
        if apps::is_frontmost(pid) {
            Ok(())
        } else {
            Err(AxError::Unsupported(
                "the target app is not frontmost; activate it before sending input",
            ))
        }
    }

    fn do_press(&mut self, target: Ref, app: &str) -> Result<ActOutcome, AxError> {
        let observed = self
            .observed_element(target.generation, target.index)
            .cloned()
            .ok_or(AxError::StaleRef)?;
        if observed.role == "securefield" {
            return Err(AxError::SecureField);
        }

        // A menu row is an index into the menu bar, not a pressable element:
        // pressing the bar item would only open the menu and leave it open.
        if let Some(path) = self.menu_path_of(target) {
            let pid = self
                .generations
                .iter()
                .find(|g| g.id == target.generation)
                .map(|g| g.pid)
                .ok_or(AxError::StaleRef)?;
            return self.do_select_menu(pid, &path, app);
        }

        let slot = self
            .slot_of(target.generation, target.index)
            .ok_or(AxError::StaleRef)?;
        let generation = self
            .generations
            .iter()
            .find(|g| g.id == target.generation)
            .ok_or(AxError::StaleRef)?;
        let elem = generation
            .store
            .get(slot as usize)
            .ok_or(AxError::StaleRef)?;

        let advertised = sys::guarded(app, "copy_action_names", || elem.actions())?;
        let Some(action) = PRESS_ACTIONS
            .iter()
            .find(|candidate| advertised.iter().any(|a| a == *candidate))
        else {
            // The CGEvent click fallback is deliberately not implemented yet;
            // failing loudly beats silently doing nothing.
            return Err(AxError::Unsupported(
                "this element advertises no AXPress/AXConfirm/AXPick/AXShowMenu action and the CGEvent click fallback is not implemented",
            ));
        };
        let name = sys::cf(action);
        sys::guarded(app, "perform_action", || elem.perform(&name))??;
        Ok(ActOutcome {
            performed: true,
            method: Method::Ax,
            relocated: false,
            summary: format!("pressed {} \"{}\"", observed.role, observed.label),
        })
    }

    fn do_set_value(&mut self, target: Ref, text: &str, app: &str) -> Result<ActOutcome, AxError> {
        let observed = self
            .observed_element(target.generation, target.index)
            .cloned()
            .ok_or(AxError::StaleRef)?;
        if observed.role == "securefield" {
            return Err(AxError::SecureField);
        }
        let slot = self
            .slot_of(target.generation, target.index)
            .ok_or(AxError::StaleRef)?;
        let generation = self
            .generations
            .iter()
            .find(|g| g.id == target.generation)
            .ok_or(AxError::StaleRef)?;
        let elem = generation
            .store
            .get(slot as usize)
            .ok_or(AxError::StaleRef)?;

        let value = sys::cf(text);
        sys::guarded(app, "set_attribute_value", || {
            elem.set_attr(&self.attrs.value, &value)
        })??;

        // `AXConfirm` where offered: some fields only commit on confirm.
        let advertised = sys::guarded(app, "copy_action_names", || elem.actions())?;
        if advertised.iter().any(|a| a == "AXConfirm") {
            let confirm = sys::cf("AXConfirm");
            let _ = sys::guarded(app, "perform_action", || elem.perform(&confirm))?;
        }

        // Read-back verification: an app that silently ignored the write must
        // not be reported as a success.
        let read_back = self.settled_value(elem, text, app)?;
        match read_back {
            Some(v) if v.trim() == text.trim() => Ok(ActOutcome {
                performed: true,
                method: Method::Ax,
                relocated: false,
                summary: format!("set {} \"{}\"", observed.role, observed.label),
            }),
            // 01 §Actions' documented fallback: a field whose `AXValue` is
            // read-only still takes typed input. A LibreOffice Calc cell is
            // exactly this — the grid exposes every cell as a text field, but
            // only typing puts a value in one.
            _ => self.type_into(slot, text, app, &observed),
        }
    }

    /// Read a field's value back until it matches `text`, or until the
    /// settle window runs out.
    ///
    /// Both write paths need this. An app republishes `AXValue` on its own
    /// run loop, so neither an `AXValue` write nor a synthesized keystroke is
    /// guaranteed to be readable on the very next accessibility call — a
    /// WebKit-backed field (Neo's own desktop composer is one) answers a
    /// frame or two later. Reading once called a landed write a failure, and
    /// on [`Self::do_set_value`] that verdict is not inert: it falls through
    /// to typing the same text again on top of the value that did land.
    fn settled_value(
        &self,
        elem: &AxElem,
        text: &str,
        app: &str,
    ) -> Result<Option<String>, AxError> {
        let deadline = Instant::now() + TYPED_VALUE_WAIT;
        loop {
            let seen = sys::guarded(app, "read_back", || {
                elem.attr(&self.attrs.value)
                    .and_then(|v| sys::as_string(&v))
            })?;
            let settled = seen
                .as_deref()
                .is_some_and(|value| value.trim() == text.trim());
            if settled || Instant::now() >= deadline {
                return Ok(seen);
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    /// Focus one element and type into it, then read back. The last resort of
    /// `set_value`, and the only path for an app whose `AXValue` is read-only.
    fn type_into(
        &mut self,
        slot: u32,
        text: &str,
        app: &str,
        observed: &Element,
    ) -> Result<ActOutcome, AxError> {
        let pid = self.generations.back().ok_or(AxError::StaleRef)?.pid;
        self.require_frontmost(pid)?;
        let generation = self.generations.back().ok_or(AxError::StaleRef)?;
        let elem = generation
            .store
            .get(slot as usize)
            .ok_or(AxError::StaleRef)?;
        // Focus first: a typed event goes wherever the focus is, so an
        // unfocusable target must fail here rather than type into whatever
        // happened to be focused instead.
        let yes = sys::cf_true();
        sys::guarded(app, "set_focused", || {
            elem.set_attr(&self.attrs.focused, yes)
        })??;
        let focused = sys::guarded(app, "read_focused", || {
            elem.attr(&self.attrs.focused)
                .and_then(|v| sys::as_bool(&v))
        })?;
        if focused != Some(true) {
            return Err(AxError::Unsupported(
                "the field neither took the value nor accepted focus",
            ));
        }
        // Focus is not always selection: a spreadsheet grid moves its own
        // selection, and a typed event goes to the selection. Pressing the
        // element first is what makes a LibreOffice Calc cell receive the
        // text, and is harmless on a field that is already focused.
        //
        // Setting `AXSelected` on the cell was tried here and **does not
        // work**: LibreOffice accepts the write, the grid selection does not
        // move, and the extra round trip made the actor miss its deadline on a
        // sheet-sized tree ("uncertain mutation: the accessibility actor could
        // not answer"). Writing a *named* cell therefore still has no AX path
        // — see 01 §spreadsheets. `AXSelected` is not it.
        let advertised = sys::guarded(app, "copy_action_names", || elem.actions())?;
        if advertised.iter().any(|action| action == "AXPress") {
            let press = sys::cf("AXPress");
            let _ = sys::guarded(app, "perform_action", || elem.perform(&press))?;
        }
        input::type_text(text, Target::Pid(pid), &self.kill)?;
        let elem = self
            .generations
            .back()
            .and_then(|g| g.store.get(slot as usize))
            .ok_or(AxError::StaleRef)?;
        let read_back = self.settled_value(elem, text, app)?;
        match read_back {
            Some(v) if v.trim() == text.trim() => Ok(ActOutcome {
                performed: true,
                method: Method::CgEvent,
                relocated: false,
                summary: format!("typed into {} \"{}\"", observed.role, observed.label),
            }),
            // An app that shows the text in an open editor rather than in
            // `AXValue` is common (a spreadsheet cell mid-edit), but this
            // actor will not call an unverifiable write a success: the caller
            // gets a refusal naming the likely cause, and the next
            // observation tells it what actually happened.
            _ => Err(AxError::Unsupported(
                "typed into the field but the value did not appear; the app may need a commit key",
            )),
        }
    }

    fn do_select_option(
        &mut self,
        target: Ref,
        option: &str,
        app: &str,
    ) -> Result<ActOutcome, AxError> {
        let slot = self
            .slot_of(target.generation, target.index)
            .ok_or(AxError::StaleRef)?;
        let generation = self
            .generations
            .iter()
            .find(|g| g.id == target.generation)
            .ok_or(AxError::StaleRef)?;
        let elem = generation
            .store
            .get(slot as usize)
            .ok_or(AxError::StaleRef)?;

        let picked = sys::guarded(app, "select_option", || {
            sys::press_child_titled(elem, &self.attrs, option)
        })?;
        if picked {
            return Ok(ActOutcome {
                performed: true,
                method: Method::Ax,
                relocated: false,
                summary: format!("selected \"{option}\""),
            });
        }
        // Fall back to writing the value directly (combo boxes take a string).
        let value = sys::cf(option);
        sys::guarded(app, "set_attribute_value", || {
            elem.set_attr(&self.attrs.value, &value)
        })??;
        Ok(ActOutcome {
            performed: true,
            method: Method::Ax,
            relocated: false,
            summary: format!("selected \"{option}\""),
        })
    }

    fn do_select_menu(
        &mut self,
        pid: i32,
        path: &[String],
        app: &str,
    ) -> Result<ActOutcome, AxError> {
        if path.is_empty() {
            return Err(AxError::Unsupported("an empty menu path selects nothing"));
        }
        let app_elem = AxElem::app(pid);
        app_elem.set_messaging_timeout(sys::APP_MESSAGING_TIMEOUT);
        sys::guarded(app, "select_menu", || {
            sys::press_menu_path(&app_elem, &self.attrs, path)
        })??;
        Ok(ActOutcome {
            performed: true,
            method: Method::Ax,
            relocated: false,
            summary: format!("chose menu {}", path.join(" › ")),
        })
    }

    fn do_scroll(
        &mut self,
        direction: crate::types::ScrollDir,
        target: Option<Ref>,
        pid: i32,
        app: &str,
    ) -> Result<ActOutcome, AxError> {
        // AX first: scrolling an element into view needs no synthetic event.
        if let Some(target) = target {
            let slot = self
                .slot_of(target.generation, target.index)
                .ok_or(AxError::StaleRef)?;
            let generation = self
                .generations
                .iter()
                .find(|g| g.id == target.generation)
                .ok_or(AxError::StaleRef)?;
            let elem = generation
                .store
                .get(slot as usize)
                .ok_or(AxError::StaleRef)?;
            let action = sys::cf("AXScrollToVisible");
            if sys::guarded(app, "scroll_to_visible", || elem.perform(&action))?.is_ok() {
                return Ok(ActOutcome {
                    performed: true,
                    method: Method::Ax,
                    relocated: false,
                    summary: "scrolled the element into view".to_owned(),
                });
            }
        }

        self.require_frontmost(pid)?;
        let height = self
            .generations
            .back()
            .and_then(|g| g.scroll_area)
            .map_or(400.0, |r| r.h);
        // 80 % of the visible height, in ~20 pt lines.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a line count derived from a window height is far inside i32"
        )]
        let lines = ((height * 0.8) / 20.0).clamp(1.0, 60.0) as i32;
        let signed = match direction {
            crate::types::ScrollDir::Up => lines,
            crate::types::ScrollDir::Down => -lines,
        };
        input::scroll_wheel(signed, Target::Pid(pid))?;
        Ok(ActOutcome {
            performed: true,
            method: Method::CgEvent,
            relocated: false,
            summary: format!("scrolled {direction:?} by {lines} lines"),
        })
    }

    // -- generation bookkeeping -------------------------------------------

    fn slot_of(&self, generation: u32, index: u16) -> Option<u32> {
        let g = self.generations.iter().find(|g| g.id == generation)?;
        g.slots.get(index as usize).copied()
    }

    /// The menu path behind a `MENU` row, if this index is one.
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

    fn read_slot(&self, generation: u32, slot: u32, app: &str) -> Option<RawNode> {
        let g = self.generations.iter().find(|g| g.id == generation)?;
        let elem = g.store.get(slot as usize)?;
        let node = sys::read_shallow(elem, &self.attrs, app);
        // An element whose role no longer reads at all is dead.
        (!node.role.is_empty()).then_some(node)
    }

    /// Relocate a dead element by fingerprint, exactly once.
    ///
    /// Exactly one match proceeds; zero or several is a stale ref. A match
    /// whose label differs is impossible by construction: the label is part
    /// of the fingerprint.
    fn relocate(&mut self, generation: u32, index: u16, app: &str) -> bool {
        let Some(g) = self.generations.iter().find(|g| g.id == generation) else {
            return false;
        };
        let Some(wanted) = g.fingerprints.get(index as usize).cloned() else {
            return false;
        };
        let pid = g.pid;

        let app_elem = AxElem::app(pid);
        app_elem.set_messaging_timeout(sys::APP_MESSAGING_TIMEOUT);
        let deadline = Instant::now() + TABLE_DEADLINE;
        let Ok(Some(window_value)) = sys::guarded(app, "focused_window", || {
            app_elem.attr(&self.attrs.focused_window)
        }) else {
            return false;
        };
        let Some(window_elem) = sys::first_element(&window_value) else {
            return false;
        };
        let walk = sys::walk(&window_elem, &self.attrs, app, deadline);

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
        let Some(found) = matches.first().and_then(|id| walk.store.get(*id as usize)) else {
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
        *entry = found.duplicate();
        true
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
#[expect(clippy::unwrap_used, reason = "tests opt out of unwrap_used (05 §2)")]
mod tests {
    use super::*;

    /// The handle must be usable from tokio and from several tasks at once.
    #[test]
    fn handle_is_send_clone_and_static() {
        fn assert_send<T: Send + Clone + 'static>() {}
        assert_send::<AxHandle>();
    }

    /// No public type may carry an `AXUIElement` or anything else `!Send`.
    #[test]
    fn public_payloads_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ElementTable>();
        assert_send::<Guard>();
        assert_send::<AxAction>();
        assert_send::<ActOutcome>();
        assert_send::<AxError>();
        assert_send::<Freshness>();
    }

    /// Listing apps needs no accessibility grant, so this runs anywhere.
    #[tokio::test]
    async fn the_actor_answers_and_shuts_down_cleanly() {
        let ax = AxHandle::spawn().unwrap();
        let apps = ax.apps().await.unwrap();
        assert!(!apps.is_empty());
        // Our own process is never listed.
        let own = std::process::id().cast_signed();
        assert!(!apps.iter().any(|a| a.pid == own));
        drop(ax);
    }

    #[tokio::test]
    async fn a_dead_handle_reports_actor_dead_rather_than_hanging() {
        let ax = AxHandle::spawn().unwrap();
        let clone = ax.clone();
        drop(ax);
        drop(clone);
        // Both clones are gone; a fresh handle is needed. Nothing to assert
        // beyond "this returned", which is the point: no hang.
    }

    #[tokio::test]
    async fn a_denied_app_is_refused_before_any_ax_read() {
        let mut policy = AxPolicy::new(std::process::id().cast_signed());
        policy.extra.push("Finder".into());
        let ax = AxHandle::spawn_with_policy(policy).unwrap();
        let err = ax.table(&AppSel::Name("Finder".into())).await.unwrap_err();
        assert!(
            matches!(err, AxError::Denied { .. } | AxError::NoApp { .. }),
            "expected a refusal, got {err:?}"
        );
        let listed = ax.apps().await.unwrap();
        assert!(!listed.iter().any(|a| a.name == "Finder"));
    }

    /// A stop has to be both immediate and final: the whole point is that a
    /// user who hits the kill switch is not driven again a moment later by a
    /// command that was already queued.
    #[tokio::test]
    async fn a_stopped_actor_refuses_every_later_command() {
        let ax = AxHandle::spawn().unwrap();
        assert!(!ax.is_stopped());
        assert!(!ax.apps().await.unwrap().is_empty());

        ax.stop();

        assert!(ax.is_stopped());
        assert!(matches!(ax.apps().await, Err(AxError::Stopped)));
        assert!(matches!(
            ax.activate(&AppSel::Name("Finder".into())).await,
            Err(AxError::Stopped)
        ));
        // And it stays stopped: there is no resume.
        assert!(matches!(ax.apps().await, Err(AxError::Stopped)));
    }

    #[tokio::test]
    async fn a_locked_screen_is_accepted_without_blocking() {
        let ax = AxHandle::spawn().unwrap();
        ax.set_locked(true);
        // The actor is still answering after handling the flag.
        assert!(!ax.apps().await.unwrap().is_empty());
        ax.set_locked(false);
    }

    #[tokio::test]
    async fn acting_with_no_observation_is_a_stale_ref() {
        let ax = AxHandle::spawn().unwrap();
        let err = ax
            .act(&AxAction::Press {
                target: Ref {
                    generation: 9,
                    index: 0,
                },
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                AxError::StaleRef | AxError::NotTrusted | AxError::ScreenLocked
            ),
            "expected a refusal, got {err:?}"
        );
    }

    #[tokio::test]
    async fn a_guard_on_an_unknown_generation_is_stale() {
        let ax = AxHandle::spawn().unwrap();
        let guard = Guard {
            generation: 12_345,
            pid: std::process::id().cast_signed(),
            window: "window|x".to_owned(),
            modal: false,
            target: None,
            element: None,
            enabled: true,
            frame: Rect::default(),
        };
        let freshness = ax.guard(&guard).await.unwrap();
        assert_eq!(freshness, Freshness::Stale(StaleReason::Generation));
    }

    // -- live tests --------------------------------------------------------
    //
    // These need the Accessibility grant and a real app. Run them with:
    //
    //   cargo test -p neo-ax -- --ignored --test-threads=1
    //
    // and make sure the binary that runs them is trusted: under `cargo test`
    // that is your **terminal**, so grant Terminal (or iTerm) Accessibility
    // in System Settings › Privacy & Security › Accessibility first.

    /// Activate TextEdit, observe it, and check the menu bar and a text area
    /// are both there.
    ///
    /// `cargo test -p neo-ax -- --ignored textedit`
    #[tokio::test]
    #[ignore = "needs the Accessibility grant and launches TextEdit"]
    async fn live_textedit_table_has_a_menu_bar_and_a_text_area() {
        assert!(
            AxHandle::trusted(),
            "grant Accessibility to the terminal running cargo test"
        );
        let ax = AxHandle::spawn().unwrap();
        let app = ax.activate(&AppSel::Name("TextEdit".into())).await.unwrap();
        assert!(app.frontmost);

        let table = ax.table(&AppSel::Pid(app.pid)).await.unwrap();
        assert!(table.elements.len() <= crate::table::MAX_ELEMENTS);
        assert!(
            table
                .elements
                .iter()
                .any(|e| e.container.as_deref() == Some("menu bar")),
            "the menu bar must be readable without opening a menu"
        );
        assert!(
            table
                .elements
                .iter()
                .any(|e| e.role == "textarea" || e.role == "scrollarea"),
            "TextEdit's document area must be visible: {:?}",
            table.elements.iter().map(|e| &e.role).collect::<Vec<_>>()
        );
        let json = serde_json::to_string(&table).unwrap();
        assert!(!json.contains("\"frame\""));
    }

    /// A guard taken right after an observation, with nothing touched in
    /// between, must be fresh.
    ///
    /// `cargo test -p neo-ax -- --ignored live_guard`
    #[tokio::test]
    #[ignore = "needs the Accessibility grant and launches TextEdit"]
    async fn live_guard_is_fresh_immediately_after_observing() {
        assert!(
            AxHandle::trusted(),
            "grant Accessibility to the terminal running cargo test"
        );
        let ax = AxHandle::spawn().unwrap();
        let app = ax.activate(&AppSel::Name("TextEdit".into())).await.unwrap();
        let table = ax.table(&AppSel::Pid(app.pid)).await.unwrap();
        let guard = table.guard_window();
        assert_eq!(ax.guard(&guard).await.unwrap(), Freshness::Fresh);
    }

    /// `select_menu` walking the menu bar by title path, verified by the
    /// window the menu item creates.
    ///
    /// `cargo test -p neo-ax -- --ignored live_select_menu`
    #[tokio::test]
    #[ignore = "needs the Accessibility grant and opens a TextEdit window"]
    async fn live_select_menu_opens_a_new_window() {
        assert!(
            AxHandle::trusted(),
            "grant Accessibility to the terminal running cargo test"
        );
        let ax = AxHandle::spawn().unwrap();
        let app = ax.activate(&AppSel::Name("TextEdit".into())).await.unwrap();
        let before = ax.table(&AppSel::Pid(app.pid)).await.unwrap().window.title;

        let outcome = ax
            .act(&AxAction::SelectMenu {
                path: vec!["File".to_owned(), "New".to_owned()],
            })
            .await
            .unwrap();
        assert!(outcome.performed);
        assert_eq!(outcome.method, Method::Ax);
        std::thread::sleep(Duration::from_millis(800));

        let after = ax.table(&AppSel::Pid(app.pid)).await.unwrap().window.title;
        assert_ne!(
            before, after,
            "File > New must put a different window in front"
        );

        // Leave no window behind, so the other live tests see a clean app.
        let _ = ax
            .act(&AxAction::SelectMenu {
                path: vec!["File".to_owned(), "Close".to_owned()],
            })
            .await;
        std::thread::sleep(Duration::from_millis(500));
    }

    /// A menu path that does not exist fails before anything is pressed.
    ///
    /// `cargo test -p neo-ax -- --ignored live_bad_menu`
    #[tokio::test]
    #[ignore = "needs the Accessibility grant and launches TextEdit"]
    async fn live_bad_menu_path_is_refused() {
        assert!(
            AxHandle::trusted(),
            "grant Accessibility to the terminal running cargo test"
        );
        let ax = AxHandle::spawn().unwrap();
        let app = ax.activate(&AppSel::Name("TextEdit".into())).await.unwrap();
        let _ = ax.table(&AppSel::Pid(app.pid)).await.unwrap();
        let err = ax
            .act(&AxAction::SelectMenu {
                path: vec!["File".to_owned(), "No Such Item".to_owned()],
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AxError::Unsupported(_)), "got {err:?}");
    }

    /// Typing into TextEdit's document and reading it back through a second,
    /// independent observation.
    ///
    /// `cargo test -p neo-ax -- --ignored live_type`
    #[tokio::test]
    #[ignore = "needs the Accessibility grant, launches TextEdit and types into it"]
    async fn live_type_text_reaches_the_document() {
        assert!(
            AxHandle::trusted(),
            "grant Accessibility to the terminal running cargo test"
        );
        let ax = AxHandle::spawn().unwrap();
        let app = ax.activate(&AppSel::Name("TextEdit".into())).await.unwrap();
        let table = ax.table(&AppSel::Pid(app.pid)).await.unwrap();
        let Some(area) = table.elements.iter().find(|e| e.role == "textarea") else {
            panic!("TextEdit needs an open document window");
        };
        // A marker unique to this run, so a leftover document from an
        // earlier run cannot make the assertion pass by accident.
        let marker = format!(
            "neo-ax {}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        ax.act(&AxAction::SetValue {
            target: area.reference(),
            text: marker.clone(),
        })
        .await
        .unwrap();

        let again = ax.table(&AppSel::Pid(app.pid)).await.unwrap();
        assert_eq!(
            again.window.title, table.window.title,
            "the window must not have changed"
        );
        assert!(
            again
                .elements
                .iter()
                .any(|e| e.value.as_deref() == Some(marker.as_str())),
            "the document should hold what we wrote"
        );
    }
}
