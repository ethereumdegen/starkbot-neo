-- A conversation is now a live agentic thread, not only the voice log 0001
-- sketched: it needs its own clock, roles for the model and its tools, a place
-- for per-message metadata, and one row per model round trip for a cost view.

-- `created_at` says what `started_at` always meant; `updated_at` is bumped by
-- every append so "the most recent thread" is one indexed read. The digest
-- columns (03 §5.2) are untouched.
ALTER TABLE conversations RENAME COLUMN started_at TO created_at;
ALTER TABLE conversations ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
UPDATE conversations SET updated_at = COALESCE(ended_at, created_at);
CREATE INDEX conversations_recent ON conversations (updated_at DESC);

-- `messages` is rebuilt rather than altered: both CHECK constraints change.
-- `role` gains the model's own turns ('bot' becomes 'assistant') and tool
-- results; `kind` keeps only the kinds something constructs today. `meta`
-- carries model/usage/tool metadata as JSON, and `at` becomes `created_at` to
-- match the index the thread is read through.
CREATE TABLE messages_v4 (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('user','assistant','tool','system')),
  source TEXT NOT NULL CHECK (source IN ('voice','typed','system')),
  kind TEXT NOT NULL CHECK (kind IN ('text','ask','result','answer')),
  text TEXT NOT NULL,
  meta TEXT CHECK (meta IS NULL OR json_valid(meta)),
  utterance_id TEXT REFERENCES utterances ON DELETE SET NULL,
  task_id TEXT,
  intent TEXT CHECK (intent IN ('new_task','amend','answer','question','cancel','chatter')),
  intent_p REAL,
  disposition TEXT CHECK (disposition IN ('enqueued','offered','ignored','forced','delivered')),
  verdict_id TEXT,
  attachments TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(attachments)),
  spoken INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  UNIQUE (conversation_id, seq)
) STRICT;

INSERT INTO messages_v4(id, conversation_id, seq, role, source, kind, text, meta, utterance_id,
                        task_id, intent, intent_p, disposition, verdict_id, attachments, spoken,
                        created_at)
SELECT id, conversation_id, seq,
       CASE role WHEN 'bot' THEN 'assistant' ELSE role END,
       source, kind, text, NULL, utterance_id,
       task_id, intent, intent_p, disposition, verdict_id, attachments, spoken, at
FROM messages;

-- Dropping the table drops its three FTS triggers with it; the external-content
-- index survives and is rebuilt against the new rowids below.
DROP TABLE messages;
ALTER TABLE messages_v4 RENAME TO messages;
CREATE INDEX messages_thread ON messages (conversation_id, created_at, seq);
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text); END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text); END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text); INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text); END;
INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');

-- One row per model round trip. Not a duplicate of `messages`: a turn that
-- produced three messages is one cost, and a turn that failed produced none.
-- `usage` is the vendor's object verbatim, so a later cost view reads fields
-- (`cache_creation_input_tokens`, `service_tier`) no normalised subset kept.
CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations ON DELETE CASCADE,
  model TEXT NOT NULL,
  provider TEXT NOT NULL,
  duration_ms INTEGER NOT NULL CHECK (duration_ms >= 0),
  usage TEXT CHECK (usage IS NULL OR json_valid(usage)),
  started_at INTEGER NOT NULL
) STRICT;
CREATE INDEX turns_thread ON turns (conversation_id, started_at);
