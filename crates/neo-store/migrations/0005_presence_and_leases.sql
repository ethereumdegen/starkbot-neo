-- Cross-process coordination for every Starkbot Neo on this machine.
--
-- Several processes share one data directory: `neo tui`, the desktop app, a
-- `neo eval` run, a one-off `neo app …`. They also share things there is
-- exactly one of on a laptop — the keyboard, the frontmost application, the
-- managed Chrome profile — so two of them driving TextEdit at once type into
-- each other's windows. The store is already the shared, WAL-backed,
-- multi-process-safe thing they all open, so it carries both the roster and
-- the mutual exclusion rather than a second daemon.

-- Who is running. One row per live process, refreshed by a heartbeat.
CREATE TABLE sessions (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL CHECK (kind IN ('tui','desktop','cli','eval','test')),
  pid         INTEGER NOT NULL,
  host        TEXT NOT NULL,
  -- One line of plain words: what this process is doing right now. Never a
  -- credential, and never a full prompt.
  activity    TEXT,
  started_at  INTEGER NOT NULL,
  last_seen   INTEGER NOT NULL
) STRICT;

CREATE INDEX sessions_live ON sessions (last_seen DESC);

-- Exclusive claims on the things there is only one of.
--
-- `resource` is a coarse name, deliberately: `keyboard`, `chrome`,
-- `app:TextEdit`. Fine-grained claims would let two processes believe they
-- can share a window.
CREATE TABLE leases (
  resource    TEXT PRIMARY KEY,
  holder      TEXT NOT NULL REFERENCES sessions ON DELETE CASCADE,
  -- Why it is held, for the message the loser is shown.
  reason      TEXT,
  acquired_at INTEGER NOT NULL,
  -- A holder that crashes must not block the machine for ever, so every
  -- lease expires and is renewed by the heartbeat.
  expires_at  INTEGER NOT NULL
) STRICT;

CREATE INDEX leases_holder ON leases (holder);
