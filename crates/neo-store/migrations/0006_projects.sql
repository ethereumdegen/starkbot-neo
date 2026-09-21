-- Named standing work and its per-project heartbeat clock.
CREATE TABLE projects (
  slug                    TEXT PRIMARY KEY,
  name                    TEXT NOT NULL,
  root                    TEXT NOT NULL UNIQUE,
  heartbeat_enabled       INTEGER NOT NULL DEFAULT 0 CHECK (heartbeat_enabled IN (0, 1)),
  heartbeat_every_seconds INTEGER NOT NULL DEFAULT 14400
                           CHECK (heartbeat_every_seconds BETWEEN 300 AND 604800),
  on_gate                 TEXT NOT NULL DEFAULT 'hold' CHECK (on_gate IN ('hold', 'skip')),
  last_tick_at            INTEGER,
  next_due_at             INTEGER,
  consecutive_failures    INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
  created_at              INTEGER NOT NULL,
  updated_at              INTEGER NOT NULL
) STRICT;

CREATE INDEX projects_due
  ON projects (heartbeat_enabled, next_due_at)
  WHERE heartbeat_enabled = 1;

CREATE TABLE heartbeat_ticks (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  project     TEXT NOT NULL REFERENCES projects(slug) ON DELETE CASCADE,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER NOT NULL,
  outcome     TEXT NOT NULL CHECK (outcome IN ('done','skipped','held','failed','dropped')),
  reason      TEXT,
  task_id     TEXT,
  goal_bytes  INTEGER NOT NULL CHECK (goal_bytes >= 0)
) STRICT;

CREATE INDEX heartbeat_ticks_project
  ON heartbeat_ticks (project, started_at DESC, id DESC);
