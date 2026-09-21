CREATE TABLE spend_events_v3 (
  id TEXT PRIMARY KEY,
  at INTEGER NOT NULL,
  day TEXT NOT NULL,
  task_id TEXT REFERENCES tasks ON DELETE SET NULL,
  kind TEXT NOT NULL CHECK (kind IN ('sol','text_helper','stt','tts','jev','fal','quiver','pack')),
  provider TEXT NOT NULL,
  model TEXT,
  units TEXT NOT NULL CHECK (json_valid(units)),
  usd REAL,
  pricing TEXT NOT NULL CHECK (pricing IN ('exact','estimated','unpriced')),
  CHECK ((pricing = 'unpriced' AND usd IS NULL) OR (pricing <> 'unpriced' AND usd >= 0))
) STRICT;

INSERT INTO spend_events_v3(id, at, day, task_id, kind, provider, model, units, usd, pricing)
SELECT id, at, day, task_id, kind, provider, model, units, usd,
       CASE exact WHEN 1 THEN 'exact' ELSE 'estimated' END
FROM spend_events;

DROP TABLE spend_events;
ALTER TABLE spend_events_v3 RENAME TO spend_events;

CREATE TABLE models_v3 (
  provider TEXT NOT NULL,
  scope TEXT NOT NULL DEFAULT 'global',
  id TEXT NOT NULL,
  use_case TEXT NOT NULL,
  capabilities TEXT NOT NULL CHECK (json_valid(capabilities)),
  price TEXT CHECK (json_valid(price)),
  price_source TEXT,
  hidden INTEGER NOT NULL DEFAULT 0,
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  PRIMARY KEY (provider, scope, id)
) STRICT;

INSERT INTO models_v3(provider, scope, id, use_case, capabilities, price, price_source, hidden, first_seen, last_seen)
SELECT provider, 'global', id, use_case, capabilities, price, price_source, hidden, first_seen, last_seen
FROM models;

DROP TABLE models;
ALTER TABLE models_v3 RENAME TO models;
