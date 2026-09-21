//! Who else is running, and who is allowed to touch the keyboard.
//!
//! Several Starkbot processes share one data directory on a laptop — a TUI, the
//! desktop app, a `neo eval` run, a one-off `neo app …`. They also share
//! things there is exactly **one** of: the keyboard and the frontmost
//! application (every `neo-ax` action is a global CGEvent or an `AXPress` on
//! whatever is in front), and the managed Chrome profile (a second launch on
//! the same profile directory either fails or steals the session).
//!
//! Two agents driving TextEdit at the same time do not produce two documents;
//! they produce one document with both their keystrokes interleaved. So this
//! module provides two things:
//!
//! * a **roster** — every live process, what it is doing, when it was last
//!   seen — so a front end can say "the desktop app is running an eval"; and
//! * **leases** — an exclusive, expiring claim on a named resource, so the
//!   second process to want the keyboard is told who has it instead of
//!   fighting for it.
//!
//! The store is the coordination point because it is already the shared,
//! WAL-backed, multi-process-safe thing all of them open. A lease is one
//! `INSERT … ON CONFLICT` guarded by an expiry, which SQLite makes atomic
//! across processes; a broker daemon would add a moving part and a new failure
//! mode for the same guarantee.
//!
//! Nothing here holds a credential. `activity` and `reason` are plain words
//! written by the process itself.

use neo_core::TimestampMs;
use rusqlite::{OptionalExtension, params};

use crate::{ReadPool, Result, Writer};

/// What kind of process a session belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    /// `neo tui`.
    Tui,
    /// The Tauri desktop app.
    Desktop,
    /// A one-shot CLI command.
    Cli,
    /// A `neo eval` run.
    Eval,
    /// A test harness.
    Test,
}

impl SessionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Desktop => "desktop",
            Self::Cli => "cli",
            Self::Eval => "eval",
            Self::Test => "test",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tui" => Some(Self::Tui),
            "desktop" => Some(Self::Desktop),
            "cli" => Some(Self::Cli),
            "eval" => Some(Self::Eval),
            "test" => Some(Self::Test),
            _ => None,
        }
    }

    /// How a front end names it to the user.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tui => "terminal",
            Self::Desktop => "desktop app",
            Self::Cli => "command",
            Self::Eval => "eval run",
            Self::Test => "test",
        }
    }
}

/// One live Starkbot process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub kind: SessionKind,
    pub pid: i32,
    pub host: String,
    /// One line of plain words: what it is doing right now.
    pub activity: Option<String>,
    pub started_at: TimestampMs,
    pub last_seen: TimestampMs,
}

impl Session {
    /// Whether this row is this very process.
    #[must_use]
    pub fn is_self(&self, my_id: &str) -> bool {
        self.id == my_id
    }
}

/// An exclusive claim on something there is only one of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub resource: String,
    pub holder: String,
    pub reason: Option<String>,
    pub acquired_at: TimestampMs,
    pub expires_at: TimestampMs,
}

/// The named things a lease can cover.
///
/// Coarse on purpose. A finer claim — "the File menu of TextEdit" — would let
/// two processes each believe they had exclusive use of the same window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Synthetic keyboard and mouse events, and the frontmost application.
    /// Every `neo-ax` action needs this.
    Keyboard,
    /// The managed Chrome profile.
    Chrome,
    /// One application, by the selector the caller used.
    App(String),
}

impl Resource {
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Keyboard => "keyboard".to_owned(),
            Self::Chrome => "chrome".to_owned(),
            Self::App(app) => format!("app:{}", app.to_lowercase()),
        }
    }

    /// How a front end names it in "X is busy" wording.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Keyboard => "the keyboard".to_owned(),
            Self::Chrome => "the managed browser".to_owned(),
            Self::App(app) => app.clone(),
        }
    }
}

/// Presence and leases.
pub struct PresenceRepository {
    writer: Writer,
    readers: ReadPool,
}

impl PresenceRepository {
    pub(crate) fn new(writer: Writer, readers: ReadPool) -> Self {
        Self { writer, readers }
    }

    /// Announce this process, or refresh it if the id is already there.
    pub fn announce(
        &self,
        id: &str,
        kind: SessionKind,
        pid: i32,
        host: &str,
        at: TimestampMs,
    ) -> Result<Session> {
        let session = Session {
            id: id.to_owned(),
            kind,
            pid,
            host: host.to_owned(),
            activity: None,
            started_at: at,
            last_seen: at,
        };
        let row = session.clone();
        self.writer.execute(move |connection| {
            connection.execute(
                "INSERT INTO sessions(id, kind, pid, host, activity, started_at, last_seen) \
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6) \
                 ON CONFLICT(id) DO UPDATE SET last_seen = excluded.last_seen",
                params![
                    row.id,
                    row.kind.as_str(),
                    row.pid,
                    row.host,
                    row.started_at,
                    row.last_seen
                ],
            )?;
            Ok(())
        })?;
        Ok(session)
    }

    /// Say what this process is doing, and stay on the roster.
    ///
    /// The same call renews every lease this session holds: a process that is
    /// alive enough to report its activity is alive enough to keep the
    /// keyboard, and one that has stopped reporting must not keep it.
    pub fn heartbeat(
        &self,
        id: &str,
        activity: Option<&str>,
        at: TimestampMs,
        lease_ttl_ms: i64,
    ) -> Result<()> {
        let id = id.to_owned();
        let activity = activity.map(str::to_owned);
        self.writer.execute(move |connection| {
            connection.execute(
                "UPDATE sessions SET last_seen = ?2, activity = COALESCE(?3, activity) \
                 WHERE id = ?1",
                params![id, at, activity],
            )?;
            connection.execute(
                "UPDATE leases SET expires_at = ?2 WHERE holder = ?1",
                params![id, at.saturating_add(lease_ttl_ms)],
            )?;
            Ok(())
        })
    }

    /// Every session seen within `stale_after_ms`, newest first.
    ///
    /// A process that crashed stops heartbeating, so it drops off by itself;
    /// the row is also removed, because a roster that grows for ever is a log,
    /// not a roster.
    pub fn sessions(&self, now: TimestampMs, stale_after_ms: i64) -> Result<Vec<Session>> {
        let cutoff = now.saturating_sub(stale_after_ms);
        self.writer.execute(move |connection| {
            connection.execute("DELETE FROM sessions WHERE last_seen < ?1", params![cutoff])?;
            Ok(())
        })?;
        self.readers.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, kind, pid, host, activity, started_at, last_seen \
                 FROM sessions WHERE last_seen >= ?1 ORDER BY last_seen DESC",
            )?;
            let rows = statement
                .query_map(params![cutoff], |row| {
                    let kind: String = row.get(1)?;
                    Ok(Session {
                        id: row.get(0)?,
                        kind: SessionKind::parse(&kind).unwrap_or(SessionKind::Cli),
                        pid: row.get(2)?,
                        host: row.get(3)?,
                        activity: row.get(4)?,
                        started_at: row.get(5)?,
                        last_seen: row.get(6)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Take an exclusive claim, or report who has it.
    ///
    /// `Ok(None)` means somebody else holds it and their claim has not
    /// expired — the caller is expected to say so, not to proceed. Re-taking a
    /// lease this session already holds succeeds and extends it, so a nested
    /// call cannot deadlock against itself.
    pub fn acquire(
        &self,
        resource: &Resource,
        holder: &str,
        reason: Option<&str>,
        at: TimestampMs,
        ttl_ms: i64,
    ) -> Result<Option<Lease>> {
        let lease = Lease {
            resource: resource.key(),
            holder: holder.to_owned(),
            reason: reason.map(str::to_owned),
            acquired_at: at,
            expires_at: at.saturating_add(ttl_ms),
        };
        let row = lease.clone();
        // One statement: SQLite serialises writers, so the conflict clause is
        // the whole mutual exclusion. Two processes racing produce one winner
        // and one `changes() == 0`.
        let taken = self.writer.execute(move |connection| {
            let changed = connection.execute(
                "INSERT INTO leases(resource, holder, reason, acquired_at, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(resource) DO UPDATE SET \
                   holder = excluded.holder, \
                   reason = excluded.reason, \
                   acquired_at = excluded.acquired_at, \
                   expires_at = excluded.expires_at \
                 WHERE leases.holder = excluded.holder OR leases.expires_at < ?4",
                params![
                    row.resource,
                    row.holder,
                    row.reason,
                    row.acquired_at,
                    row.expires_at
                ],
            )?;
            Ok(changed > 0)
        })?;
        if taken { Ok(Some(lease)) } else { Ok(None) }
    }

    /// Who holds this resource right now, if anyone.
    pub fn holder(&self, resource: &Resource, now: TimestampMs) -> Result<Option<Lease>> {
        let key = resource.key();
        self.readers.read(move |connection| {
            let lease = connection
                .query_row(
                    "SELECT resource, holder, reason, acquired_at, expires_at FROM leases \
                     WHERE resource = ?1 AND expires_at >= ?2",
                    params![key, now],
                    |row| {
                        Ok(Lease {
                            resource: row.get(0)?,
                            holder: row.get(1)?,
                            reason: row.get(2)?,
                            acquired_at: row.get(3)?,
                            expires_at: row.get(4)?,
                        })
                    },
                )
                .optional()?;
            Ok(lease)
        })
    }

    /// Give a claim back. Releasing something this session does not hold is
    /// not an error: the lease may already have expired.
    pub fn release(&self, resource: &Resource, holder: &str) -> Result<()> {
        let key = resource.key();
        let holder = holder.to_owned();
        self.writer.execute(move |connection| {
            connection.execute(
                "DELETE FROM leases WHERE resource = ?1 AND holder = ?2",
                params![key, holder],
            )?;
            Ok(())
        })
    }

    /// Leave the roster, dropping every lease this session held.
    pub fn depart(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.writer.execute(move |connection| {
            // `leases.holder` cascades, so one delete is enough.
            connection.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    // A failed `expect` in a test is the test failing, which is the point.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let store = Store::open(dir.path().join("neo.db"), dir.path()).expect("the store opens");
        (dir, store)
    }

    /// The roster is the point of this module: a second process has to be able
    /// to see the first one and say what it is doing.
    #[test]
    fn a_session_appears_on_the_roster_with_what_it_is_doing() {
        let (_dir, store) = store();
        let presence = store.presence();
        presence
            .announce("a", SessionKind::Tui, 101, "laptop", 1_000)
            .expect("announce");
        presence
            .heartbeat("a", Some("driving TextEdit"), 1_500, 30_000)
            .expect("heartbeat");

        let sessions = presence.sessions(2_000, 90_000).expect("roster");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].kind, SessionKind::Tui);
        assert_eq!(sessions[0].pid, 101);
        assert_eq!(sessions[0].activity.as_deref(), Some("driving TextEdit"));
    }

    /// A process that stopped heartbeating is gone, however it died.
    #[test]
    fn a_session_that_stopped_heartbeating_drops_off() {
        let (_dir, store) = store();
        let presence = store.presence();
        presence
            .announce("gone", SessionKind::Eval, 7, "laptop", 1_000)
            .expect("announce");
        assert_eq!(presence.sessions(1_000, 90_000).expect("roster").len(), 1);
        // Well past the staleness window.
        assert!(
            presence
                .sessions(1_000 + 90_001, 90_000)
                .expect("roster")
                .is_empty()
        );
    }

    /// The whole reason leases exist: the second claimant must lose, not
    /// share. Two agents on one keyboard produce one document with both their
    /// keystrokes in it.
    #[test]
    fn only_one_session_can_hold_the_keyboard() {
        let (_dir, store) = store();
        let presence = store.presence();
        for (id, kind) in [("a", SessionKind::Tui), ("b", SessionKind::Desktop)] {
            presence
                .announce(id, kind, 1, "laptop", 1_000)
                .expect("announce");
        }

        let first = presence
            .acquire(&Resource::Keyboard, "a", Some("typing"), 1_000, 30_000)
            .expect("acquire");
        assert!(first.is_some());
        let second = presence
            .acquire(&Resource::Keyboard, "b", Some("also typing"), 1_100, 30_000)
            .expect("acquire");
        assert!(second.is_none(), "two holders of one keyboard");

        // And the loser can find out who has it, to say so.
        let holder = presence
            .holder(&Resource::Keyboard, 1_100)
            .expect("holder")
            .expect("someone holds it");
        assert_eq!(holder.holder, "a");
        assert_eq!(holder.reason.as_deref(), Some("typing"));
    }

    /// Re-taking a lease you already hold extends it, so a nested call cannot
    /// deadlock a process against itself.
    #[test]
    fn a_holder_can_retake_its_own_lease() {
        let (_dir, store) = store();
        let presence = store.presence();
        presence
            .announce("a", SessionKind::Cli, 1, "laptop", 1_000)
            .expect("announce");
        assert!(
            presence
                .acquire(&Resource::Keyboard, "a", None, 1_000, 30_000)
                .expect("acquire")
                .is_some()
        );
        let again = presence
            .acquire(&Resource::Keyboard, "a", None, 1_010, 30_000)
            .expect("acquire")
            .expect("the same holder may retake it");
        assert_eq!(again.expires_at, 31_010);
    }

    /// A crashed holder must not block the machine for ever.
    #[test]
    fn an_expired_lease_can_be_taken_by_someone_else() {
        let (_dir, store) = store();
        let presence = store.presence();
        for id in ["a", "b"] {
            presence
                .announce(id, SessionKind::Cli, 1, "laptop", 1_000)
                .expect("announce");
        }
        presence
            .acquire(&Resource::Keyboard, "a", None, 1_000, 30_000)
            .expect("acquire");
        // Before expiry: refused. After: granted.
        assert!(
            presence
                .acquire(&Resource::Keyboard, "b", None, 20_000, 30_000)
                .expect("acquire")
                .is_none()
        );
        assert!(
            presence
                .acquire(&Resource::Keyboard, "b", None, 31_001, 30_000)
                .expect("acquire")
                .is_some()
        );
    }

    /// Leaving drops the leases with the session, so a clean exit does not
    /// make the next process wait out an expiry.
    #[test]
    fn departing_releases_every_lease() {
        let (_dir, store) = store();
        let presence = store.presence();
        presence
            .announce("a", SessionKind::Tui, 1, "laptop", 1_000)
            .expect("announce");
        presence
            .acquire(&Resource::Keyboard, "a", None, 1_000, 30_000)
            .expect("acquire");
        presence.depart("a").expect("depart");
        assert!(
            presence
                .holder(&Resource::Keyboard, 1_100)
                .expect("holder")
                .is_none()
        );
    }

    /// Different apps are different resources; the same app named two ways is
    /// one, because the case of a bundle id or a display name is not a
    /// distinction the laptop makes.
    #[test]
    fn app_leases_are_per_app_and_case_insensitive() {
        assert_eq!(
            Resource::App("TextEdit".into()).key(),
            Resource::App("textedit".into()).key()
        );
        assert_ne!(
            Resource::App("TextEdit".into()).key(),
            Resource::App("LibreOffice".into()).key()
        );
        assert_ne!(Resource::Keyboard.key(), Resource::Chrome.key());
    }
}
