//! The screen lease: one run at a time may drive the keyboard.
//!
//! Driving an application means taking the keyboard and the frontmost window,
//! and a machine has exactly one of each. Two runs overlapping do not produce
//! two half-finished results; they produce one wrong one, because keystrokes
//! land in whichever window came forward last and the navigator's next
//! observation describes a screen the other run just changed. `neo-eval`
//! already knew this and pinned itself to `concurrency: 1`, but that only
//! stopped a suite racing *itself*.
//!
//! # Why a file lock and not a mutex
//!
//! The surfaces that can start this work are separate processes: the TUI, the
//! desktop app, the CLI, and the daemon. A `Mutex` inside [`Runtime`] would
//! serialise one of them against itself and leave the interesting case — the
//! user starting `:nav` in the TUI while the desktop app drives TextEdit —
//! completely unguarded. So the lease is an `flock` on
//! `<data_dir>/screen.lock`, which every process that shares a data directory
//! contends for.
//!
//! An `flock` also self-heals. The kernel releases it when the holding
//! process dies, however it died, so a crashed or killed run cannot wedge the
//! machine — which a lock file holding a PID, checked by hand, always
//! eventually does.
//!
//! # Why nesting is explicit
//!
//! A suite holds the screen across its cases, and each case then runs an app
//! turn that wants the screen too. That nesting is legitimate: the work
//! already owns the keyboard and is subdividing its own turn. `flock` cannot
//! express it — two file descriptors in one process contend exactly as two
//! processes do — so it is tracked in memory *above* the file lock.
//!
//! What it is tracked *by* is a [`ScreenScope`] the holder hands down, not
//! "this process already holds it". That older rule excluded nothing within a
//! process: two unrelated runs in one TUI were both granted the keyboard and
//! typed into each other's window, which is the exact failure the lease
//! exists to prevent. The rule is now stated in terms that are true: **work
//! that presents the live hold's scope, or that runs under the same `RunId`,
//! may subdivide that hold; everything else is refused exactly as another
//! process is.**
//!
//! # What it deliberately does not cover
//!
//! A headless browser run takes no screen and no lease — it is the common
//! case and serialising it would be a lie about what it needs. Read-only
//! accessibility queries (`trusted`, `apps`, `table`) take none either: they
//! observe, and an observation that races an action was already stale by the
//! time it was rendered, which `neo-ax` reports through its own freshness
//! guard rather than by locking.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use neo_core::RunId;
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The lock file, beside the store whose runs contend for it.
const LOCK_FILE: &str = "screen.lock";

/// Identifies one live hold, so work nested inside it can prove it belongs.
///
/// A hold cannot be recognised from the acquiring [`RunId`]. The eval suite
/// holds the screen across its cases and each case runs an agent turn under a
/// run id of its own, and that id is what separates the per-case traces, the
/// `turns` row and the desktop's run registry — collapsing them would cost
/// more than the lease is worth. Nor can nesting be inferred from "this
/// process already holds it": that is the permissive rule that let two
/// unrelated runs in one process type at once.
///
/// So it is passed. A holder hands its scope to the work it starts
/// ([`ScreenGuard::scope`]), and [`ScreenLease::acquire_within`] grants an
/// acquisition that presents the scope of the hold that is actually live.
/// There is no public constructor and the counter is process-local, so a
/// scope can be neither forged nor carried in from another process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenScope(u64);

/// The next scope number. Monotonic, so a scope from a hold that has already
/// ended never matches the one that is live now.
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);

/// Who holds the screen, recorded in the lock file so a refusal can say so.
///
/// Written by the holder and read only by a process that has just been
/// refused, which is why a torn or absent record is not an error: the answer
/// "someone else is driving, and here is what little I know" is still the
/// right answer, and inventing certainty about a foreign process is not worth
/// a single line of parsing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    /// The surface that started it (`neo-tui`, `neo-desktop`, `neo-cli`), so a
    /// user who forgot a window has somewhere to look.
    pub surface: String,
    pub run: RunId,
    /// What the run is doing, in the words the refusal will use: `"drive
    /// TextEdit"`, `"eval suite"`, `"navigate example.com"`.
    pub what: String,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is running `{}` (pid {}, run {})",
            self.surface, self.what, self.pid, self.run
        )
    }
}

/// The screen is taken.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ScreenBusy {
    /// Another process holds the lease and said who it is.
    #[error("the screen is in use: {0}. Stop that run, or wait for it to finish.")]
    HeldBy(Holder),
    /// Another process holds the lease and left no readable record — it was
    /// mid-write, or it predates this format. The refusal still stands.
    #[error(
        "the screen is in use by another Starkbot process. Stop that run, or wait for it to finish."
    )]
    HeldByUnknown,
}

impl ScreenBusy {
    /// The holder, when one was readable.
    #[must_use]
    pub fn holder(&self) -> Option<&Holder> {
        match self {
            ScreenBusy::HeldBy(holder) => Some(holder),
            ScreenBusy::HeldByUnknown => None,
        }
    }
}

/// Shared lease state for one process. Held by [`Runtime`]; there is one per
/// data directory because that is what the lock file is keyed on.
#[derive(Clone)]
pub struct ScreenLease {
    path: PathBuf,
    surface: String,
    state: Arc<Mutex<Option<Held>>>,
}

/// What this process currently holds. `depth` counts nested acquisitions; the
/// file lock is taken once, at depth 1, and released when it returns to 0.
struct Held {
    file: File,
    holder: Holder,
    depth: usize,
    /// What nested work has to present to be let in. Minted when the file
    /// lock is taken, so it names this hold and no later one.
    scope: ScreenScope,
}

impl ScreenLease {
    #[must_use]
    pub fn new(data_dir: &Path, surface: impl Into<String>) -> Self {
        Self {
            path: data_dir.join(LOCK_FILE),
            surface: surface.into(),
            state: Arc::new(Mutex::new(None)),
        }
    }

    /// Take the screen for `run`, or say who has it.
    ///
    /// Non-blocking on purpose: a caller that waited would leave a user
    /// staring at a UI that says nothing while another window types. The
    /// refusal names the holder so the front end can offer to stop it.
    ///
    /// Work that is *part of* a hold this process already has presents that
    /// hold's [`ScreenScope`] through [`ScreenLease::acquire_within`]; this
    /// entry point presents none and is therefore refused while any other
    /// run holds the screen.
    pub fn acquire(&self, run: RunId, what: impl Into<String>) -> Result<ScreenGuard, ScreenBusy> {
        self.acquire_within(None, run, what)
    }

    /// [`ScreenLease::acquire`] for work nested inside a hold that already
    /// exists.
    ///
    /// `scope` is the scope of the hold this work belongs to, or `None` when
    /// it belongs to none. A nested acquisition that presents the live hold's
    /// scope — or that runs under the holder's own `RunId` — is granted and
    /// counted, so the screen stays held until every guard is dropped.
    /// Everything else is refused exactly as another process is, including a
    /// second, unrelated run in this process: `flock` is per open file
    /// description, so without this the file lock excluded every process
    /// except the one that could actually interleave keystrokes.
    pub fn acquire_within(
        &self,
        scope: Option<ScreenScope>,
        run: RunId,
        what: impl Into<String>,
    ) -> Result<ScreenGuard, ScreenBusy> {
        let what = what.into();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            // A panic while holding the lease must not make the screen
            // permanently unavailable: the file lock is the real guard, and it
            // is still correct.
            poisoned.into_inner()
        });

        if let Some(held) = state.as_mut() {
            if scope != Some(held.scope) && held.holder.run != run {
                return Err(ScreenBusy::HeldBy(held.holder.clone()));
            }
            // The holder subdividing its own work. See the module docs.
            held.depth += 1;
            return Ok(ScreenGuard {
                lease: self.clone(),
                scope: held.scope,
            });
        }

        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .map_err(|_| ScreenBusy::HeldByUnknown)?;

        if flock(&file, FlockOperation::NonBlockingLockExclusive).is_err() {
            // Someone else has it. Read their record for the message, and
            // treat anything unreadable as "held, details unknown" — the
            // refusal does not depend on parsing a foreign write.
            return Err(
                read_holder(&self.path).map_or(ScreenBusy::HeldByUnknown, |holder| {
                    ScreenBusy::HeldBy(holder)
                }),
            );
        }

        let holder = Holder {
            pid: std::process::id(),
            surface: self.surface.clone(),
            run,
            what,
            since: OffsetDateTime::now_utc(),
        };
        let scope = ScreenScope(NEXT_SCOPE.fetch_add(1, Ordering::Relaxed));
        // Best effort: the lock is what excludes, the record only explains.
        // A failed write costs a helpful message, not correctness.
        write_holder(&file, &holder);
        *state = Some(Held {
            file,
            holder,
            depth: 1,
            scope,
        });

        Ok(ScreenGuard {
            lease: self.clone(),
            scope,
        })
    }

    /// Who holds the screen right now: this process's own record, or whatever
    /// the lock file says when another process holds it. `None` means free.
    #[must_use]
    pub fn holder(&self) -> Option<Holder> {
        if let Some(held) = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            return Some(held.holder.clone());
        }
        // Free as far as this process knows. Probe the file: a successful
        // shared lock means nobody holds it, so the record (if any) is stale.
        let Ok(file) = File::open(&self.path) else {
            return None;
        };
        if flock(&file, FlockOperation::NonBlockingLockShared).is_ok() {
            let _ = flock(&file, FlockOperation::Unlock);
            return None;
        }
        read_holder(&self.path)
    }

    fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(held) = state.as_mut() else { return };
        held.depth -= 1;
        if held.depth > 0 {
            return;
        }
        if let Some(held) = state.take() {
            // Clear the record before the lock goes, so a reader that wins the
            // race sees an empty file rather than a holder that has left.
            let _ = held.file.set_len(0);
            drop(held.file); // closing the descriptor releases the flock
        }
    }
}

/// Proof that the holder may drive the screen. Releasing is `Drop`, so a
/// `?` on any step in between cannot leak the lease.
pub struct ScreenGuard {
    lease: ScreenLease,
    scope: ScreenScope,
}

impl ScreenGuard {
    /// The token to hand to work that runs *inside* this hold, so that work
    /// is granted the screen instead of refused. See [`ScreenScope`].
    #[must_use]
    pub fn scope(&self) -> ScreenScope {
        self.scope
    }
}

impl std::fmt::Debug for ScreenGuard {
    /// Names the hold rather than the plumbing: a test that prints this wants
    /// to know who has the screen, not that a `File` exists.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.lease.holder() {
            Some(holder) => write!(f, "ScreenGuard({holder})"),
            None => f.write_str("ScreenGuard(released)"),
        }
    }
}
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        self.lease.release();
    }
}

fn write_holder(mut file: &File, holder: &Holder) {
    let Ok(json) = serde_json::to_vec(holder) else {
        return;
    };
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.write_all(&json);
    let _ = file.flush();
}

fn read_holder(path: &Path) -> Option<Holder> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;

    fn lease(dir: &Path, surface: &str) -> ScreenLease {
        ScreenLease::new(dir, surface)
    }

    /// Two leases over one data directory are two contenders, exactly as two
    /// processes are: `flock` is per open file description, so this is the
    /// cross-process case a unit test can actually reach.
    #[test]
    fn a_second_holder_is_refused_and_told_who_has_it() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let first = lease(dir.path(), "neo-tui");
        let second = lease(dir.path(), "neo-desktop");

        let run = RunId::new();
        let _held = first.acquire(run, "drive TextEdit").expect("the screen");

        let refused = second
            .acquire(RunId::new(), "eval suite")
            .expect_err("the second run must be refused");
        let holder = refused.holder().expect("the refusal names the holder");
        assert_eq!(holder.surface, "neo-tui");
        assert_eq!(holder.what, "drive TextEdit");
        assert_eq!(holder.run, run);
        assert_eq!(holder.pid, std::process::id());
        // The message is the whole point of recording a holder.
        assert!(refused.to_string().contains("drive TextEdit"));
    }

    #[test]
    fn releasing_lets_the_next_run_in() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let first = lease(dir.path(), "neo-tui");
        let second = lease(dir.path(), "neo-desktop");

        drop(
            first
                .acquire(RunId::new(), "drive TextEdit")
                .expect("first"),
        );
        second
            .acquire(RunId::new(), "eval suite")
            .expect("the screen is free again");
    }

    /// A suite holds the screen and each of its cases runs an app turn under
    /// a run id of its own. The nesting has to work — eval deadlocks against
    /// itself otherwise — and it works because the suite hands its scope
    /// down, not because the two happen to share a process.
    #[test]
    fn work_that_presents_the_live_scope_may_subdivide_the_hold() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let mine = lease(dir.path(), "neo-cli");
        let theirs = lease(dir.path(), "neo-desktop");

        let suite = mine.acquire(RunId::new(), "eval suite").expect("suite");
        let case = mine
            .acquire_within(Some(suite.scope()), RunId::new(), "drive TextEdit")
            .expect("a case inside the suite's lease is allowed");

        // Still exclusive to this process while nested.
        assert!(theirs.acquire(RunId::new(), "navigate").is_err());

        drop(case);
        // The outer hold survives the inner release — otherwise the suite
        // would lose the screen after its first case.
        assert!(theirs.acquire(RunId::new(), "navigate").is_err());

        drop(suite);
        theirs
            .acquire(RunId::new(), "navigate")
            .expect("released at depth zero");
    }

    /// The case the lease existed for and did not cover. `flock` is per open
    /// file description, so before the scope was passed explicitly *any*
    /// second acquisition in the holding process was granted — two runs in one
    /// TUI typed into each other's window while the file lock held off every
    /// process that was not going to interfere.
    #[test]
    fn a_second_unrelated_run_in_the_same_process_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let lease = lease(dir.path(), "neo-tui");

        let driving = RunId::new();
        let held = lease
            .acquire(driving, "drive TextEdit")
            .expect("the screen");

        let refused = lease
            .acquire(RunId::new(), "drive Keynote")
            .expect_err("an unrelated run must be refused, process or not");
        let holder = refused.holder().expect("the refusal names the holder");
        assert_eq!(holder.run, driving);
        assert!(refused.to_string().contains("drive TextEdit"));

        // A scope that is not the live one is worth no more than none at all.
        drop(held);
        let next = lease
            .acquire(RunId::new(), "eval suite")
            .expect("free again");
        let stale = next.scope();
        drop(next);
        let other = lease.acquire(RunId::new(), "drive Numbers").expect("free");
        assert!(
            lease
                .acquire_within(Some(stale), RunId::new(), "drive Keynote")
                .is_err(),
            "a scope from a hold that has ended must not open the one that is live"
        );
        drop(other);
    }

    /// The holder's own run may re-enter without carrying a token: a run that
    /// has the keyboard subdividing itself is the same work, and `crate::ax`
    /// takes the screen again inside a run that already holds it.
    #[test]
    fn the_holders_own_run_may_re_enter_without_a_scope() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let lease = lease(dir.path(), "neo-cli");

        let run = RunId::new();
        let outer = lease.acquire(run, "drive TextEdit").expect("the screen");
        let inner = lease
            .acquire(run, "read TextEdit")
            .expect("the same run may subdivide its own work");

        drop(inner);
        assert_eq!(
            lease.holder().map(|holder| holder.run),
            Some(run),
            "the inner release must not drop a hold the outer run still has"
        );
        drop(outer);
        assert_eq!(lease.holder(), None);
    }

    /// The record explains; the lock excludes. A leftover record with no live
    /// lock — a process killed before it could clear it — must not stop the
    /// next run.
    #[test]
    fn a_stale_record_without_a_lock_does_not_hold_the_screen() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let stale = Holder {
            pid: 999_999,
            surface: "neo-desktop".to_owned(),
            run: RunId::new(),
            what: "drive TextEdit".to_owned(),
            since: OffsetDateTime::now_utc(),
        };
        std::fs::write(
            dir.path().join(LOCK_FILE),
            serde_json::to_vec(&stale).expect("json"),
        )
        .expect("write the stale record");

        let lease = lease(dir.path(), "neo-tui");
        assert_eq!(lease.holder(), None, "nobody holds an unlocked file");
        lease
            .acquire(RunId::new(), "eval suite")
            .expect("a stale record must not wedge the machine");
    }

    #[test]
    fn holder_reports_this_processs_own_hold_and_nothing_when_free() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let lease = lease(dir.path(), "neo-tui");
        assert_eq!(lease.holder(), None);

        let held = lease.acquire(RunId::new(), "drive TextEdit").expect("hold");
        let holder = lease.holder().expect("held");
        assert_eq!(holder.what, "drive TextEdit");

        drop(held);
        assert_eq!(lease.holder(), None);
    }
}
