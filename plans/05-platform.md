# 05 — Platform: workspace, data, keys, permissions, shipping, testing

Everything the other docs stand on. The constitution (00) wins; this doc owns the workspace layout, the SQLite schema, the on-disk layout, settings, `neo-keys`, the model registry, macOS permissions and session state, signing and updates, the dev loop, observability, the security summary, tests, CI and the M1 build order. Items marked *(verify)* are API choices not confirmed against a primary source; M0/M1 resolves them.

## 1. Workspace

```
starkbot-neo/
  Cargo.toml  rust-toolchain.toml  deny.toml  clippy.toml  .github/workflows/
  crates/
    neo-keys/  neo-core/  neo-store/  jev-nav/  neo-ax/  neo-voice/  neo-judge/  neo-packs/
    neo-media/  neo-canvas/  neo-canvas-agent/  neo-agent/  neo-cli/
  src-tauri/          the app shell (tauri.conf.json, Info.plist, entitlements*.plist, capabilities/)
  ui/                 React + TS + Vite (entries: main, pill, ring, quick-entry); pnpm
  fixtures/           eval JSONL, raw AX trees, WAVs, fixture sites, recorded sessions, injection pages, seed DBs per schema version
  plans/
```

| Crate | Responsibility |
|---|---|
| `neo-core` | Shared vocabulary as types: ids (UUIDv7), `Task`, `Message`, `Utterance`, `TraceItem`, `AppEvent`, `Settings` (+ defaults, validation, patch), errors, the three provider traits with `ModelRef` / `Endpoint` / `Usage` (08), the model registry's pure logic (classify, resolve, hide-list) and the `PriceTable` type. No I/O beyond serde. |
| `neo-store` | The only crate that speaks SQL: connection setup + pragmas, the writer actor and read pool, migrations, typed repositories per table group, retention/pruning, zstd for large bodies, backup-before-migrate. |
| `neo-keys` | Keychain read/write/delete, the `Secret` newtype, `KeyAccount`, `KeyStatus`, the `KeyValidator` trait, env fallback. The only place a secret string exists. Leaf crate: no `neo-*` dependencies, no HTTP, no vendor URLs. |
| `jev-nav` | The navigator: `wire` (the one TypeSafe client — A6), `policy`, `rules`, `text` helper, `Observer` trait, `web/` (`CdpObserver`), `ax/` (`AxObserver`, feature `ax`). Standalone and publishable: depends on no `neo-*` crate except `neo-ax` under `ax`. |
| `neo-ax` | macOS accessibility actor for native apps (one run-loop thread, batched fetch, refs, diffs, input). Also home of the two small macOS modules everything needs earlier: `perm` (Accessibility trust, M1) and `session` (lock / display sleep / user activity, M4). No `neo-*` dependencies. |
| `neo-voice` | Capture, VAD, segmenter, STT, TTS, duplex gate, mic permission (`perm`), and the `OpenAiSpeech` provider impl. |
| `neo-judge` | Intake, routing, routine match, gates and micro-edit calls built on `jev-nav::wire`; the verdict log (`jev_verdicts`) and human-signal backfill; the eval harness; the TypeSafe `KeyValidator`. |
| `neo-packs` | Pack bundle read/validate, registry of enabled packs, HTTP tool runner, routines, install + `neo.lock`, consent summary, the generic enablement flow and data-driven key validation. |
| `neo-media` | Adapter over the `degen-media-maker` lib: media tools, `MediaBackend` wiring (`fal`, `quiver`), spend estimates, studio index, fal/Quiver `KeyValidator`s. |
| `neo-canvas` | The hypercanvas document: HTML/CSS frames, node tree, ops + transactions, per-author undo, tokens, knobs, pins, storage inside the studio folder. |
| `neo-canvas-agent` | Canvas tools for Sol, outline/look, micro-edit policy, region workers, browser-engine render for export and critique. |
| `neo-agent` | Queue worker, router, Sol orchestrator on `metalcraft`, tools, `Gated<T>`, caps, trace, spend meter, the `OpenAiInference` provider impl, and **`Runtime`** — the one facade (`start`, commands in, `AppEvent` stream out) that both binaries drive. |
| `neo-cli` | The `neo` binary: every subsystem headless (`doctor`, `keys`, `models`, `voice`, `nav`, `judge`, `run`, `pack`, `media`, `canvas`, `scenario`) plus hidden `neo dev …` maintenance commands (export bindings, capture fixtures, seed DBs). |
| `src-tauri` | Tauri app shell only: typed command/event bridge over `Runtime`, windows and NSPanels, tray, global shortcuts, updater, single-instance, autostart, notifications. No product logic. |

**Allowed dependency direction** (`A ─▶ B` = A may depend on B; nothing points the other way):

```
neo-store ─▶ neo-core ─▶ neo-keys (leaf)
jev-nav (standalone) ──feature ax──▶ neo-ax (standalone)
neo-voice, neo-packs, neo-media, neo-canvas ─▶ neo-core, neo-store, neo-keys        (neo-media ─▶ degen-media-maker)
neo-judge ─▶ jev-nav, neo-core, neo-store, neo-keys
neo-canvas-agent ─▶ neo-canvas, neo-media, neo-judge, jev-nav
neo-agent ─▶ all of the above (+ metalcraft, rig)
neo-cli, src-tauri ─▶ neo-agent
```

Rules (checked in review; 1, 3, 4 also by CI):
1. **Nothing below `src-tauri` depends on Tauri** (`cargo tree -p <crate> -i tauri` must be empty for every crate but `src-tauri`). The whole product runs headless in `neo`.
2. **Secrets only in `neo-keys`.** `Secret`'s inner string is private; `Secret::expose()` is on `clippy.toml`'s `disallowed-methods` and is `#[allow]`ed at exactly four call sites: provider impls building an auth header, `jev-nav` credential adapter in `neo-judge`, the pack HTTP runner's `$VAR` expansion, and the rig client constructor in `neo-agent`. Headers carrying a key are `HeaderValue::set_sensitive(true)`.
3. **No vendor URL outside a provider impl** (or, for packs, outside pack data). CI greps for `api.openai.com`, `typesafe.ai`, `fal.run`, `quiver.ai` outside the allow-listed modules.
4. **No Python, no shell agent surface**: no `.py` file, no `python` invocation anywhere in the repo or CI (a CI step fails on either).
5. `neo-core` does no I/O; `neo-store` is the only SQL; `src-tauri` and `ui/` hold no secrets and no logic. Provider impls and HTTP clients take `{ base_url, credential }` from their provider object (08); tests point them at `wiremock`.
6. `unsafe` only in `neo-ax`, `neo-voice::perm`, `src-tauri` panel glue — `unsafe_code = "deny"` workspace-wide, `#![allow]` per module with a `// SAFETY:` comment per block.
## 2. Cargo conventions

```toml
[workspace]
resolver = "3"
members = ["crates/*", "src-tauri"]
[workspace.package]
edition = "2024"
rust-version = "1.91"
license = "MIT"
[workspace.lints]
rust = { unsafe_code = "deny", unused_must_use = "deny" }
clippy = { unwrap_used = "deny", expect_used = "warn", dbg_macro = "deny", print_stdout = "deny", disallowed_methods = "deny", await_holding_lock = "deny" }
                                # tests opt out of unwrap_used; neo-cli opts out of print_stdout
[profile.dev]
opt-level = 1
debug = "line-tables-only"
[profile.dev.package]           # pixel, DSP and compression crates are unusable unoptimised on macOS
resvg = { opt-level = 3 }       # one identical line each for: usvg, tiny-skia, fontdb, rustybuzz, image, png, webp,
                                # rubato, rustfft, earshot, zstd-sys, libsqlite3-sys
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = "debuginfo"
split-debuginfo = "packed"      # dSYM archived per release for symbolication
panic = "unwind"                # a panicking task must not take the desktop worker down
```

- Every dependency version lives in `[workspace.dependencies]`; member crates write `foo.workspace = true`. `tauri-specta` and `specta` are pinned with `=`. `metalcraft` and `degen-media-maker` are git/path dependencies until published.
- `rust-toolchain.toml` pins the stable channel (≥ 1.91) with `rustfmt` + `clippy`; targets `aarch64-apple-darwin` and `x86_64-apple-darwin`.
- One `reqwest`, one `tokio`, one `rusqlite` in `Cargo.lock` — `cargo deny check bans` fails on duplicates of those three *(verify which `reqwest` minor `rig` 0.37 / metalcraft 0.12 pull, and align on it)*.
- Feature flags are few: `jev-nav` → `web` (default), `ax` (macOS only, pulls `neo-ax`); `neo-voice` → `tts`, `live-stt`; `neo-agent` → `media`, `canvas` (default on; off for fast CLI builds). Env-key fallback is **not** a feature (feature unification would leak it into the app): it is a runtime `KeySource` chosen by each binary (§6).
- Long builds on this machine use a scratch target dir — `CARGO_TARGET_DIR=$TMPDIR/neo-target` — because other sessions sometimes delete `target/` mid-build. `neo dev env` prints the export line.

## 3. Dependencies by area (verified 2026-09-18)

| Area | Crates |
|---|---|
| App shell | `tauri` 2.11 (`tray-icon`, `macos-private-api`); plugins `global-shortcut` 2.3, `updater` 2.10, `single-instance` 2.4, `autostart` 2.5, `log` 2.9, `notification` 2.3; `tauri-nspanel` (git, branch `v2.1`, pinned by rev) |
| Bridge | `tauri-specta` =2.0.0-rc.25 + matching `specta` (exact pins); fallback if the rc bites: `ts-rs` + one hand-written `api.ts` |
| Sol · Jev | `metalcraft` 0.11 → **0.12** (feature `rig`), `rig` pinned 0.37 (upstream 0.42; the bump rides in metalcraft 0.12) · Jev needs none: `jev-nav::wire` is raw `reqwest` and **the `jev` crate is not a dependency** (A6) |
| HTTP / WS | `reqwest` (`rustls-tls`, `http2`, `multipart`, `stream`, `json`), `tokio-tungstenite` (streaming STT; CDP attach mode), `url`, `wiremock` (dev) |
| macOS | `objc2` (+`exception`), `objc2-application-services` 0.3, `objc2-core-graphics`, `objc2-core-foundation`, `objc2-app-kit`, `objc2-av-foundation`, `objc2-foundation` |
| Audio | `cpal` 0.18, `rubato` 5, `earshot`, `rtrb`, `hound` |
| Storage | `rusqlite` 0.40 (`bundled`), `rusqlite_migration`, `zstd`, `uuid` (v7), `time` |
| Secrets | `keyring` 4.2 with feature **`apple-native-keyring-store`** (not in the defaults), `zeroize` |
| Packs | `include_dir`, `jsonschema`, `semver`, `sha2`, `tar` + `flate2`, `notify` (also watches `soul.md`) |
| Media / canvas | `degen-media-maker` (lib), `resvg` / `usvg` / `tiny-skia` / `fontdb`, `webp`, `image`; the HTML/CSS parser choice belongs to 11 |
| Everywhere | `tokio`, `serde` / `serde_json`, `thiserror` (libs), `anyhow` (bins only), `tracing` + `tracing-subscriber` + `tracing-appender`, `async-trait`, `clap` (cli), `insta` / `proptest` / `tempfile` (dev) |
| UI | React 19, Vite, TypeScript, Zustand, Vitest + Testing Library |

## 4. Data

### 4.1 On-disk layout

```
~/Library/Application Support/com.starkbot.neo/
  neo.db  neo.db-wal  neo.db-shm
  soul.md                     soul.history/            last 10 saves
  packs/<id>/<version>/       neo.lock                 registries.json
  chrome-profile/             user-data-dir of Stark's Chrome (A4)
  attachments/<message-id>/   files dropped on the composer
  recordings/                 only when "keep recordings" is on: ring of the last 20 utterance WAVs
  backups/                    neo-v<user_version>-<date>.db, last 3
  run/                        worker.lock · chrome.json (pid, pipe) · ax-flags.json (crash marker, 01)
~/Library/Logs/com.starkbot.neo/  neo.<date>.log, 7 days   ·   ~/Library/Caches/com.starkbot.neo/  thumbnails, render cache, prices.json (safe to delete)
~/Documents/starkbot-neo/studios/<name>/   studio.json · takes.jsonl · takes/ · canvas/ · exports/   (A16; readable by `dmm`)
```

**Not in SQLite, by design:** studio folders (the folder is the truth; SQLite holds only an index), `soul.md` and its history (a user-editable file), pack contents (files + `neo.lock`), every secret (Keychain), audio, logs, Chrome's profile. Deleting `neo.db` loses history and settings but no studio, no key and no soul.

### 4.2 Connection rules

Pragmas on every connection: `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY`, `wal_autocheckpoint=1000`, `journal_size_limit=67108864`; at creation only: `auto_vacuum=INCREMENTAL`, `application_id=0x4E454F31` ("NEO1"). All tables `STRICT`. Ids are UUIDv7 text; times are integer Unix milliseconds UTC; `day` columns are the user's local date `YYYY-MM-DD`; JSON columns carry a `json_valid` check.

One **writer actor** (a dedicated thread owning the only read-write connection, fed by an mpsc channel, batching trace inserts into one transaction per 50 ms) plus a pool of 4 `query_only` readers. `rusqlite` never runs on a tokio worker thread. The app and `neo` may have the DB open at once (WAL), but only one process may run the desktop worker: `run/worker.lock` (`flock`) decides, and the loser runs read-only commands.

### 4.3 Schema (`migrations/0001_init.sql`)

```sql
CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;   -- created_by_version, last_opened_by_version, last_prune_at, last_backup_at
CREATE TABLE settings (section TEXT PRIMARY KEY, value TEXT NOT NULL CHECK (json_valid(value)), updated_at INTEGER NOT NULL) STRICT;
CREATE TABLE conversations (id TEXT PRIMARY KEY, title TEXT, started_at INTEGER NOT NULL, ended_at INTEGER,
  digest TEXT, digest_upto_seq INTEGER NOT NULL DEFAULT 0) STRICT;         -- digest = the ~1.5k-token carry-over for Sol
CREATE TABLE utterances (
  id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations ON DELETE CASCADE,
  started_at INTEGER NOT NULL, duration_ms INTEGER NOT NULL, stt_model TEXT, stt_ms INTEGER, text TEXT,
  disposition TEXT NOT NULL CHECK (disposition IN ('intake','prefiltered','control','answer_bypass','self_echo','stt_failed')),
  filter_reason TEXT, audio_path TEXT) STRICT;
CREATE TABLE messages (
  id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations ON DELETE CASCADE, seq INTEGER NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('user','bot')), source TEXT NOT NULL CHECK (source IN ('voice','typed','system')),
  kind TEXT NOT NULL CHECK (kind IN ('text','ask','confirm','result','answer','media','notice')),
  text TEXT NOT NULL, utterance_id TEXT REFERENCES utterances ON DELETE SET NULL, task_id TEXT,
  intent TEXT CHECK (intent IN ('new_task','amend','answer','question','cancel','chatter')), intent_p REAL,
  disposition TEXT CHECK (disposition IN ('enqueued','offered','ignored','forced','delivered')),   -- offered = "enqueue?" chip
  verdict_id TEXT, attachments TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(attachments)), spoken INTEGER NOT NULL DEFAULT 0,
  at INTEGER NOT NULL, UNIQUE (conversation_id, seq)) STRICT;
CREATE VIRTUAL TABLE messages_fts USING fts5(text, content='messages', content_rowid='rowid');      -- + ai/ad/au triggers
CREATE TABLE tasks (
  id TEXT PRIMARY KEY, conversation_id TEXT REFERENCES conversations ON DELETE SET NULL, origin_message_id TEXT,
  parent_task_id TEXT REFERENCES tasks ON DELETE SET NULL,                  -- retry-of, or the task a pin / region worker belongs to
  text TEXT NOT NULL, goal TEXT,                                            -- goal = the sentence the navigator works from (text + amendments)
  source TEXT NOT NULL CHECK (source IN ('voice','typed','retry','pin','system')),
  route TEXT NOT NULL CHECK (route IN ('navigate','design','media','multi','question','routine')), routine_ref TEXT,   -- question = the intent's own route; routine + routine_ref 'pack-id/name' = `routine:<name>`
  lane TEXT NOT NULL CHECK (lane IN ('desktop','readonly','canvas')),       -- A10 concurrency
  status TEXT NOT NULL CHECK (status IN ('queued','running','needs_confirm','waiting_user','done','failed','cancelled')),
  paused_reason TEXT CHECK (paused_reason IN ('queue_paused','screen_locked','display_asleep','user_active','daily_cap','jev_outage','key_invalid','offline')),
  fail_code TEXT, error TEXT, summary TEXT, say TEXT,                       -- fail_code: screen_locked | interrupted | step_cap | wall_cap | spend_cap | blocked | killed | …
  position REAL NOT NULL, persona TEXT, studio_id TEXT, target_app TEXT, start_url TEXT,
  pack_set TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(pack_set)),         -- enabled packs snapshotted at start (tool list never changes mid-task)
  caps TEXT NOT NULL CHECK (json_valid(caps)),                              -- caps snapshotted at start
  steps INTEGER NOT NULL DEFAULT 0, jev_calls INTEGER NOT NULL DEFAULT 0, sol_calls INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL NOT NULL DEFAULT 0, cost_exact INTEGER NOT NULL DEFAULT 0, progress REAL NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, started_at INTEGER, finished_at INTEGER) STRICT;
CREATE INDEX tasks_queue ON tasks (status, lane, position);
CREATE TABLE task_amendments (id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks ON DELETE CASCADE, message_id TEXT,
  text TEXT NOT NULL, at INTEGER NOT NULL, applied_after_seq INTEGER) STRICT;
CREATE TABLE trace_items (
  task_id TEXT NOT NULL REFERENCES tasks ON DELETE CASCADE, seq INTEGER NOT NULL, step INTEGER,
  kind TEXT NOT NULL CHECK (kind IN ('route','thought','action','steer','judgment','saw','said','asked','amended','gate','spend','pack','canvas','error')),
  at INTEGER NOT NULL, dur_ms INTEGER, enc INTEGER NOT NULL DEFAULT 0,      -- 0 = UTF-8 JSON, 1 = zstd(JSON)
  body BLOB NOT NULL, PRIMARY KEY (task_id, seq)) STRICT, WITHOUT ROWID;
CREATE TABLE task_checkpoints (task_id TEXT PRIMARY KEY REFERENCES tasks ON DELETE CASCADE, state BLOB NOT NULL, updated_at INTEGER NOT NULL) STRICT;  -- metalcraft Checkpointer (zstd); inspection only, never blind resume
CREATE TABLE jev_verdicts (
  id TEXT PRIMARY KEY, at INTEGER NOT NULL,
  caller TEXT NOT NULL CHECK (caller IN ('intake','route','routine_match','nav_step','gate','grounding','verify','micro_edit')),
  task_id TEXT REFERENCES tasks ON DELETE SET NULL, step INTEGER, message_id TEXT,
  model TEXT, rules_version TEXT, enc INTEGER NOT NULL DEFAULT 0, request BLOB,   -- exact state + heads sent; nulled by retention
  answers TEXT NOT NULL CHECK (json_valid(answers)),                        -- per head: choice, probabilities, confidence
  outcome TEXT NOT NULL CHECK (outcome IN ('ok','timeout','invalid','http_error')), latency_ms INTEGER NOT NULL,
  input_tokens INTEGER, output_tokens INTEGER,
  human_signal TEXT CHECK (human_signal IN ('approved','denied','force_enqueued','offer_accepted','cancelled_after','retried','undone','corrected')),
  human_signal_at INTEGER) STRICT;
CREATE INDEX jev_verdicts_task ON jev_verdicts (task_id, step);
CREATE TABLE confirms (
  id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks ON DELETE CASCADE, step INTEGER, verdict_id TEXT,
  raised_by TEXT NOT NULL CHECK (raised_by IN ('rule','head','spend','pack_policy','app_mode')), trigger TEXT NOT NULL,  -- e.g. label:send · outward=0.62 · est>$0.25
  action_sentence TEXT NOT NULL, scope_kind TEXT CHECK (scope_kind IN ('app','origin')), scope TEXT, label TEXT, operation TEXT,
  risk TEXT CHECK (json_valid(risk)), est_usd REAL,
  status TEXT NOT NULL CHECK (status IN ('pending','approved','denied','timed_out','cancelled')),
  resolved_via TEXT CHECK (resolved_via IN ('ui','voice','timeout','kill')), remembered INTEGER NOT NULL DEFAULT 0,
  requested_at INTEGER NOT NULL, resolved_at INTEGER) STRICT;
CREATE TABLE remembered_allows (                                            -- "always allow this in <app>"; never consulted for deny rules or `spends`
  id TEXT PRIMARY KEY, scope_kind TEXT NOT NULL CHECK (scope_kind IN ('app','origin')), scope TEXT NOT NULL,
  label_norm TEXT NOT NULL, operation TEXT NOT NULL, from_confirm_id TEXT, created_at INTEGER NOT NULL,
  last_used_at INTEGER, uses INTEGER NOT NULL DEFAULT 0, UNIQUE (scope_kind, scope, label_norm, operation)) STRICT;
CREATE TABLE app_policies (                                                 -- seeded with the default deny list (terminal-class apps, password managers, …)
  bundle_id TEXT PRIMARY KEY, name TEXT, mode TEXT NOT NULL CHECK (mode IN ('allowed','confirm_all','denied')),
  ax_strategy TEXT CHECK (ax_strategy IN ('none','manual','enhanced')), source TEXT NOT NULL, updated_at INTEGER NOT NULL) STRICT;  -- source: default | user | pack:<id>
CREATE TABLE origin_policies (pattern TEXT PRIMARY KEY, mode TEXT NOT NULL CHECK (mode IN ('allowed','confirm_all','denied')),
  max_actions_per_hour INTEGER, source TEXT NOT NULL, updated_at INTEGER NOT NULL) STRICT;
CREATE TABLE origin_activity (origin TEXT NOT NULL, hour INTEGER NOT NULL, actions INTEGER NOT NULL, PRIMARY KEY (origin, hour)) STRICT, WITHOUT ROWID;  -- pacing guard (09)
CREATE TABLE packs (
  id TEXT PRIMARY KEY, version TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 0, embedded INTEGER NOT NULL DEFAULT 0,
  source TEXT NOT NULL, sha256 TEXT NOT NULL, partially_supported INTEGER NOT NULL DEFAULT 0,
  consent TEXT NOT NULL CHECK (json_valid(consent)), installed_at INTEGER NOT NULL, enabled_at INTEGER) STRICT;
CREATE TABLE pack_keys (                                                    -- status only; the value is in the Keychain. pack_id '@core' = openai, typesafe
  pack_id TEXT NOT NULL, env_name TEXT NOT NULL, account TEXT NOT NULL, detail TEXT, checked_at INTEGER,
  status TEXT NOT NULL CHECK (status IN ('missing','present','invalid','unchecked','limited')), PRIMARY KEY (pack_id, env_name)) STRICT;
CREATE TABLE routine_runs (                                                 -- routine usage; aggregates are queries
  id TEXT PRIMARY KEY, task_id TEXT REFERENCES tasks ON DELETE SET NULL, pack_id TEXT NOT NULL, routine TEXT NOT NULL,
  match_confidence REAL, params TEXT CHECK (json_valid(params)),
  outcome TEXT NOT NULL CHECK (outcome IN ('passed','verify_failed','step_failed','handoff','cancelled')), dur_ms INTEGER, at INTEGER NOT NULL) STRICT;
CREATE TABLE spend_events (
  id TEXT PRIMARY KEY, at INTEGER NOT NULL, day TEXT NOT NULL, task_id TEXT REFERENCES tasks ON DELETE SET NULL,
  kind TEXT NOT NULL CHECK (kind IN ('sol','text_helper','stt','tts','jev','fal','quiver','pack')),
  provider TEXT NOT NULL, model TEXT, units TEXT NOT NULL CHECK (json_valid(units)),   -- tokens in/out/cached, audio seconds, chars, images, video seconds
  usd REAL NOT NULL, exact INTEGER NOT NULL) STRICT;                        -- exact = 0 → Estimated
CREATE TABLE spend_daily (day TEXT NOT NULL, kind TEXT NOT NULL, usd REAL NOT NULL, calls INTEGER NOT NULL, PRIMARY KEY (day, kind)) STRICT, WITHOUT ROWID;  -- upserted with spend_events; the daily cap reads only this
CREATE TABLE models (provider TEXT NOT NULL, id TEXT NOT NULL, use_case TEXT NOT NULL, capabilities TEXT NOT NULL CHECK (json_valid(capabilities)),
  price TEXT CHECK (json_valid(price)), price_source TEXT, hidden INTEGER NOT NULL DEFAULT 0,
  first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, PRIMARY KEY (provider, id)) STRICT;   -- registry cache; serves until a refresh lands
CREATE TABLE studios (                                                      -- an index only; the folder is the truth, rebuilt on open
  id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL UNIQUE, take_count INTEGER NOT NULL DEFAULT 0, last_take TEXT,
  frame_count INTEGER NOT NULL DEFAULT 0, brand TEXT CHECK (json_valid(brand)), missing INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, last_opened_at INTEGER) STRICT;
```

### 4.4 Migration, retention, compression

- **Migrations** are numbered SQL files embedded with `include_str!` and applied by `rusqlite_migration::to_latest()` at open (`user_version` is the version). Forward-only; a shipped migration is never edited. Before applying any pending migration: `VACUUM INTO backups/…` (keep 3). A DB whose `user_version` is newer than the binary (updater rollback) is **refused** with a clear screen, never opened read-write. Additive changes first; a destructive drop ships one release after the code stops reading the column. Tests: `Migrations::validate()`, plus upgrade from a seed DB of every released version in `fixtures/db/`.
- **zstd**: `trace_items.body`, `jev_verdicts.request`, `task_checkpoints.state` are compressed at level 3 when the JSON exceeds 8 KiB (`enc = 1`). Snapshots and page text are the bulk; typical 10–20×.
- **Retention** (Settings → Privacy; defaults): trace items 30 days (the task row and its summary stay); `jev_verdicts.request` nulled at 30 days, rows 180 days; checkpoints 7 days; ignored / prefiltered utterance text 7 days; messages until the user clears them; `spend_events` 400 days, `spend_daily` forever; `origin_activity` 48 hours. "Delete transcripts" and "Clear conversation" are hard deletes, FTS included.
- **Prune job**: at start and every 24 h, only when no task is running, in chunks of 5,000 rows, then `PRAGMA incremental_vacuum` and `wal_checkpoint(TRUNCATE)`.
- On start, any task found `running` / `needs_confirm` / `waiting_user` becomes `failed` with `fail_code = interrupted`; pending confirms become `cancelled`.

## 5. Settings

One typed `Settings` struct in `neo-core`, one row per section in `settings`. Missing sections and missing fields take `Default` — so adding a setting needs no migration.

- **Patch semantics**: `patch_settings(section, patch)` is a JSON Merge Patch (RFC 7396) over that section → deserialised into the typed section with `deny_unknown_fields` → `validate()` (ranges, enum values, floors) → written → `AppEvent::SettingsChanged { section }`. A patch that fails validation changes nothing and returns the field path and reason. `null` resets a field to its default.
- **When a change lands**: thresholds, caps, voice and hotkeys apply immediately; anything inside Sol's cacheable prefix (name, `soul.md`, persona, models, enabled packs) applies from the **next task**; caps are snapshotted per task (`tasks.caps`).
- **Floors** no patch, pack or `soul.md` can cross: confirm thresholds ≤ 0.60, `on_task` floor ≥ 0.15, `spends` confirms cannot be remembered, default-deny apps can be lifted only one at a time from Settings → Safety.

| Section | Field | Default |
|---|---|---|
| `identity` | `name` | `"Stark"` |
| `listen` | `enabled` (restored) · `addressing` · `push_to_talk` · `mic_device` | `true` · `open` · `false` · system default |
| `voice` | `tts_enabled` · `tts_voice` · `speak` · `duplex` · `keep_recordings` | `false` · `marin` · `questions_only` · `auto` · `false` |
| `models` | `inference` · `text_helper` · `stt` · `stt_live` · `tts` · `sol_effort` | `sol-latest` (today `gpt-5.6-sol`) · `gpt-5.6-luna` (reasoning off) · `gpt-transcribe` · off (`gpt-live-transcribe`) · `gpt-4o-mini-tts` · `low` |
| `intake` | `enqueue_at` · `offer_at` | 0.70 · 0.40 |
| `safety` | `confirm_at` (outward / destructive / spends) · `on_task_floor` · `confirm_timeout_s` · `confirm_labels` | 0.40 · 0.30 (twice → `BLOCKED`) · 120 (= deny) · the global list (03) |
| `caps` (K5) | `usd_per_task` · `usd_media_call_confirm` · `usd_per_day` · `sol_steps` · `nav_actions` · `nav_decisions` · `wall_minutes` | **1.00** · **0.25** · **10.00** · 40 · 60 · 120 · 10 |
| `queue` · `browser` | `idle_wait_s` before a task's first action · `paused` · `browser.mode` | 3 · `false` · `managed` (Stark's Chrome; `attach` is opt-in) |
| `hotkeys` | `toggle_listen` · `quick_entry` · `kill` | `⌥Space` · `⌥⌘Space` · `⌃⌥⌘.` |
| `general` · `privacy` | `autostart` · `update_check` · `notifications` · `trace_days` · `verdict_days` · `ignored_text_days` | `false` · `true` · `true` · 30 · 180 · 7 |

## 6. Keys (`neo-keys`)

- **Store**: Keychain generic-password items, service `com.starkbot.neo`, this device only, not iCloud-synced *(verify `keyring` 4.2 exposes the accessibility attribute; otherwise set it through `security-framework`)*. Keychain ACLs bind to the code-signing requirement — one more reason for a stable Developer ID from day one; ad-hoc rebuilds re-prompt every time.
- **Accounts are an open set**: `openai`, `typesafe` (core, K1) · `FAL_KEY`, `QUIVERAI_API_KEY` (media enablement) · any `requires_env` name a pack declares · later `starkrouter`. Pack accounts are the env name itself, so two packs that both declare `FAL_KEY` share one item — but only after the consent screen says *"share your existing FAL_KEY with <pack>"*. The core accounts are not env-style names, so no pack can name them. `$VAR` expands only for names the pack declares, and only into requests to that integration's `allowed_hosts`.
- **`Secret`**: private `String`, `Zeroize` on drop, `Debug`/`Display` print `Secret(•••• last4)`, no `Serialize`, no `Clone` outside the crate (`Arc<Secret>` is shared). `expose()` is clippy-disallowed except at the four sites in §1 rule 2. Secrets are read once and cached in-process so the Keychain is not hit per request.
- **Flow**: UI `set_key(account, value)` → Rust validates → Keychain → `pack_keys` status row → webview is told only `missing | present | invalid | unchecked | limited`. A key is never echoed back, never logged, never placed in argv (`neo keys set <account>` reads stdin with echo off), never accepted from voice or from a task.
- **Validation = one authenticated call, made by the provider impl** (so `neo-keys` holds no URL) through `trait KeyValidator { async fn validate(&self, s: &Secret) -> KeyStatus }`:

| Account | Call | Reading |
|---|---|---|
| `openai` | `GET {base}/models` | 200 → `present`; also require a `*-sol` id and `gpt-transcribe` in the list, else `limited` (project-scoped key); 401 → `invalid` |
| `typesafe` | `jev-nav::wire::ping()` — one minimal `yes_no` head | 200 + a valid answer → `present`; 401/403 → `invalid` |
| `FAL_KEY` | any authenticated fal GET | **401 → `invalid`**; a 403 from the usage endpoint is normal and counts as `present` |
| `QUIVERAI_API_KEY` | authenticated model-list GET *(verify endpoint)* | 401 → `invalid` |
| pack keys | the pack's `key_help.validate` (method, url, ok statuses); the host must be in `allowed_hosts` | none declared → `unchecked` |

  Network errors, 429 and 5xx → `unchecked`: the key is stored and retried. Re-validation: app start, every 24 h, and immediately on any 401 from that provider (the queue pauses with `paused_reason = key_invalid`).
- **Env fallback**: `KeySource::KeychainThenEnv` reads `OPENAI_API_KEY`, `TYPESAFE_API_KEY`, `FAL_KEY` (`FAL_API_KEY`), `QUIVERAI_API_KEY` (`QUIVER_API_KEY`). `neo` always uses it; the app uses it only under `cfg!(debug_assertions)`. A release app never reads a key from the environment.

## 7. Model registry

- **Refresh**: on start (non-blocking — the `models` table serves until the fetch lands), every 6 h, on key change, on Settings → Models → Refresh, and at once when any call returns model-not-found.
- **Classification** of `GET /models` ids (a provider that returns capabilities, i.e. StarkRouter, overrides the patterns):

| Use case | Pattern |
|---|---|
| inference / text helper | `^gpt-(\d+(?:\.\d+)*)-(sol\|terra\|luna)(-\d{4}-\d{2}-\d{2})?$` → `{version, tier, dated}` |
| STT · streaming STT | `^gpt-transcribe` · `^gpt-live-transcribe` |
| TTS | `-tts(-\|$)` |
| never offered | hide-list `whisper-1`, `gpt-4o-transcribe*`, `gpt-4o-mini-transcribe*`, `tts-1*` (K3); `gpt-image*`, `dall-e*` (K4); everything unclassified |

- **`sol-latest`** = the undated `gpt-*-sol` id with the highest version, compared as numeric tuples (`5.10 > 5.9`); previews and dated snapshots are excluded. Settings store the symbolic value; the UI shows the resolved id beside it. A changed resolution applies from the next task and posts one `notice` message ("Sol is now …") with the new price.
- A saved id that disappears from the list raises a warning in Settings and falls back to the use case's default; a hidden id can never be selected, even by editing the DB (validated at load). Every use case holds a `ModelRef { provider, id }` (08), never a bare string.
- **Prices are read live, never compiled in** (K3). `PriceTable` sources, in order: (1) provider-reported — exact `usage.cost` and `/models` prices from StarkRouter, per-endpoint prices from fal; (2) a dated `prices.json` fetched from the release host every 24 h, signed with the updater key and cached; (3) none → the call is recorded with units only, spend shows "unpriced", and the dollar caps fall back to the step and wall caps. Everything from (2) is `Usage::Estimated` and the UI labels it so.

## 8. Permissions and session state

| Permission | Status | Mechanics |
|---|---|---|
| **Microphone** | requested (onboarding 4) | `AVCaptureDevice authorizationStatusForMediaType:` / `requestAccessForMediaType:` via `objc2-av-foundation` in `neo-voice::perm`; `NSMicrophoneUsageDescription`; entitlement `com.apple.security.device.audio-input`. Denied → deep link `x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone` |
| **Accessibility** | requested (onboarding 5) | `AXIsProcessTrustedWithOptions(prompt)` once, then poll `AXIsProcessTrusted()` each second while the screen is open; deep link `…?Privacy_Accessibility`; `CGPreflightPostEventAccess()` for posting. Needed for native apps, `CGEvent` input and the kill switch's modifier release — not for CDP |
| Documents / Desktop / Downloads folder | prompted by macOS itself on first touch | non-sandboxed apps get a Files-and-Folders prompt: add `NSDocumentsFolderUsageDescription` (studios), `NSDesktopFolderUsageDescription`, `NSDownloadsFolderUsageDescription` (exports). Open/save panels need nothing |
| Screen Recording | **never requested** (P10) | export and critique renders come from the browser engine over CDP / `WKWebView`, not from screen capture |
| Input Monitoring | **never requested** | we post events, we never tap them; global hotkeys go through `tauri-plugin-global-shortcut` (Carbon hotkeys, no TCC) |
| Apple Events / Automation | **never requested** | no AppleScript, no `osascript`; no `NSAppleEventsUsageDescription`, no `automation.apple-events` entitlement |

TCC facts that shape the dev loop: the Accessibility grant is keyed on bundle id **+ code requirement**; an ad-hoc build loses it on every rebuild while the toggle still shows ON (fix: `tccutil reset Accessibility com.starkbot.neo`, then re-grant); under `tauri dev` / `cargo run` the grant is attributed to the **terminal**. `check_permissions()` returns `granted | denied | not_determined | stale` (`stale` = toggle on, process untrusted). A revoked permission re-opens its onboarding step and pauses the queue.

**Session state** (`neo-ax::session`, P7) publishes `SessionState { locked, display_asleep, on_console, user_idle_s }` on a `tokio::sync::watch`. Observers need a run loop: the main thread in the app, a dedicated `CFRunLoop` thread in `neo`.

| Signal | Mechanism |
|---|---|
| screen locked / unlocked | `NSDistributedNotificationCenter` `com.apple.screenIsLocked` / `com.apple.screenIsUnlocked` *(verify — undocumented but long-stable)*; initial state from `CGSessionCopyCurrentDictionary()` key `CGSSessionScreenIsLocked` *(verify)* |
| display asleep / awake | `NSWorkspace.shared.notificationCenter`: `screensDidSleep` / `screensDidWake`, `willSleep` / `didWake` (documented); cross-check `CGDisplayIsAsleep(CGMainDisplayID())` before each task |
| fast user switch · screensaver | `NSWorkspaceSessionDidResignActive` / `DidBecomeActive`, `kCGSessionOnConsoleKey` · `com.apple.screensaver.didstart` / `didstop` *(verify)* — both treated as locked |
| user activity | `CGEventSourceSecondsSinceLastEventType(HIDSystemState, kCGAnyInputEventType)` polled at 4 Hz while a desktop task is queued or running; needs no TCC grant *(verify on the current macOS; verify that events we post with a tagged `CGEventSource` do not reset the HID-state counter)* |

Policy (P7): `locked ∨ display_asleep ∨ ¬on_console` → queue pauses (`paused_reason`), Listen drops the stream, a running task stops at the next step boundary as `failed` / `screen_locked`; on unlock everything resumes after a 2 s debounce. If a signal cannot be read, the state is treated as **locked**.

## 9. Signing, notarization, updates

- **Identity**: `bundle.macOS.signingIdentity = "Developer ID Application: …"`, hardened runtime, universal binary (`--target universal-apple-darwin`), **not sandboxed**, `app.macOSPrivateApi = true`. Minimum macOS 14 *(D)*. `neo` ships inside the bundle as a Tauri `externalBin`, signed with the same identity.
- **`entitlements.plist`** (release) — exactly two keys:

```xml
<plist version="1.0"><dict>
  <key>com.apple.security.device.audio-input</key><true/>
  <key>com.apple.security.cs.allow-jit</key><true/>          <!-- WKWebView JIT under hardened runtime -->
</dict></plist>
```

  Deliberately absent: `app-sandbox`, `cs.allow-unsigned-executable-memory`, `cs.disable-library-validation`, `cs.allow-dyld-environment-variables`, `automation.apple-events`, `device.camera`. `entitlements.debug.plist` adds only `com.apple.security.get-task-allow` (notarization rejects it, so it never reaches a release).
- **Notarization**: on tags only, with an App Store Connect API key (`APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH`) → `notarytool` → staple the `.app` and the DMG. Gate: `spctl -a -vv` and `stapler validate` in CI.
- **DMG**: Tauri's bundler, background + Applications alias. First launch outside `/Applications` offers to move itself (TCC and the updater both want a stable path).
- **Updater**: `tauri signer generate` keypair; the public key in `tauri.conf.json`; the private key + password in CI secrets **and two offline backups — losing it strands every install**. Static `latest.json` + `.app.tar.gz` + `.sig` on the release host. Check on launch and every 24 h; download in the background; **install only when no task is running, the queue is empty or paused, and no confirm is pending**, then relaunch restoring Listen. Same Developer ID + bundle id ⇒ TCC grants and Keychain access survive updates.
- **Single instance**: `tauri-plugin-single-instance` registered first; a second launch focuses `main` and exits. **Autostart**: `tauri-plugin-autostart` (LaunchAgent), off by default; a login launch starts hidden with the pill, restoring the last Listen state.

## 10. Dev workflow

| Mode | Use for | TCC / Keychain |
|---|---|---|
| **`neo` CLI** (`cargo run -p neo-cli -- …`) | the daily loop for everything below the UI; every milestone goes green here first | grants attributed to the terminal — run from Terminal.app/iTerm, not an IDE terminal; keys from env |
| **`cargo tauri dev`** | UI work with hot reload | same as above; panels and permissions prompts are not representative |
| **Signed debug build** (`cargo tauri build --debug`, Developer ID, copied to `/Applications`) | anything touching TCC, Keychain ACLs, NSPanels over fullscreen, updater, autostart | the real grant; survives rebuilds because the code requirement is stable |

**`neo doctor`** (also the Settings → Doctor tab; exit code ≠ 0 on any red): macOS version and arch · running from `/Applications`? · signature: identity, team id, hardened runtime, entitlements match the expected two · Accessibility `granted / stale` (+ the `tccutil` line) · `CGPreflightPostEventAccess` · Microphone status · input device present and default sample rate · Keychain read/write round-trip · each key's `KeyStatus` · OpenAI reachable, `/models` latency, `sol-latest` resolution, configured ids present and not hidden · TypeSafe reachable + `ping` latency · `prices.json` age and signature · DB opens, `user_version`, `integrity_check`, WAL size, free disk · `worker.lock` holder · Chrome found, version, managed profile launches with a debugging pipe · session state readable (lock, display, idle) · secure input currently on? · `ffmpeg` on PATH (media) · fonts present · enabled packs validate, missing pack keys · updater endpoint reachable and public key present · log dir writable.

## 11. Observability

- `tracing` everywhere; `tracing-subscriber` with a daily-rolling `tracing-appender` file (7 days) and, in `neo`, a pretty stderr layer. `tauri-plugin-log` forwards webview console output into the same file. Release builds compile out `trace`/`debug` (`release_max_level_info`).
- **Span layout**: `task{id, route, lane}` › `step{n}` › `observe` · `rules` · `jev{caller, heads}` · `text_helper` · `confirm_wait` · `execute` · `settle` · `verify`; Sol: `sol_call{model}` › `tool{name}`; voice: `vad_cut` › `stt` › `intake`; media: `media_call{backend, endpoint}`; canvas: `canvas_tx{author}` · `render`. Span durations are the single source for `trace_items.dur_ms`, the status strip's step latency and the numbers in `plans/spikes.md`.
- **Redaction**: `Secret` cannot be formatted; auth headers are sensitive; request/response bodies, page text, transcripts, `soul.md` and typed values are never logged at `info` — logs carry ids, sizes, latencies and outcomes, and the content lives only in the trace (SQLite, user-deletable).
- **Diagnostics bundle** (Doctor → "Save diagnostics…", or `neo doctor --bundle`): a `.tar.gz` written where the user chooses through a save panel — logs of the last 48 h, the doctor report as JSON, settings with paths and names stripped, schema version and table row counts, pack list with versions, the last 20 tasks as `{route, status, fail_code, steps, durations}`. Never included: the DB, message or task text, traces, `soul.md`, keys. Nothing is ever uploaded by the app.

## 12. Security posture

| Threat | Control |
|---|---|
| **Prompt injection via page / screen text** | page text is data in a delimited field, never instructions (rule block in every request); model output is only ever an index into observed controls; `on_task` head (< 0.30 twice → `BLOCKED`); deterministic confirm labels + `outward` / `destructive` / `spends` heads → confirm card; the bot works only in tabs it opened; no shell, no file tools, terminal-class apps denied (P3); injection fixtures gate every release |
| **Malicious pack** | packs are data, nothing executes; validation + consent screen (domains, keys, mutating tools, routine steps, policies); `allowed_hosts` enforced on the parsed URL, no redirects; `$VAR` only for the pack's own declared names; policies tighten-only; skill text is advice that still passes every gate; sha256 in `neo.lock`, updates show a consent diff |
| **Key exfiltration** | Keychain only; `Secret` redaction + clippy-fenced `expose()`; the webview sees status only and has no HTTP capability to vendor hosts (CSP + Tauri capabilities allow IPC only); keys never typed, spoken, shown or sent in a task; secure fields never read; no key in argv, logs, diagnostics or crash output |
| **Mis-hearing** | pre-intake filter → Jev intake with an "enqueue?" band (0.40–0.70) → every outward / destructive / spending step still confirms; heard text is always visible and one click to undo or cancel; local `stop` vocabulary and the kill hotkey work with no network; typed and spoken tasks get identical gates |
| **Runaway spend** | per-task cap, per-media-call confirm, daily cap (K5) — enforced from `spend_daily` before each paid call, with the estimate in the action sentence; step / decision / wall caps as the backstop when a price is unknown; pacing guard per origin; StarkRouter later adds server-side per-key limits |
| **Supply chain** | `Cargo.lock` + `pnpm-lock.yaml` committed; `cargo deny` (advisories, licences, bans, sources) and `cargo audit` in CI; git dependencies pinned by rev; hardened runtime with library validation on; updater artifacts signed with the offline-backed key, `prices.json` with the same key; Developer ID + notarization; no runtime code download, no plugins that execute |
| **Unattended Mac · provider outage** | P7 policy (§8), fail-closed on unreadable session state · risk decisions fail closed (→ confirm or pause), intake fails open to the UI ("enqueue?"), banner + `paused_reason` |

## 13. Tests — Rust only

**No Python is used anywhere in this project** — not in tests, fixtures, spikes, release tooling or CI steps. Test harnesses, fixture servers, fixture capture and release helpers are Rust (`cargo test`, `neo dev …`); UI tests are TypeScript; CI YAML contains only one-line `cargo` / `pnpm` / Apple tool invocations.

| Crate | How it is tested |
|---|---|
| `neo-core` | unit: settings defaults / merge-patch / floors (`proptest`), registry classification and `sol-latest` ordering over recorded `/models` payloads, `AppEvent` serde snapshots (`insta`) |
| `neo-store` | in-memory + temp-file DBs: migrations validate, upgrade from every seed DB, repositories, zstd round-trip, retention on a synthetic year, writer-actor ordering, two-process lock |
| `neo-keys` | `Secret` never appears in `Debug`/`Display`/serde (compile-fail + runtime tests); Keychain round-trip under a test service name (macOS runner only); env fallback matrix |
| `jev-nav` | the reference's offline suite ported 1:1 (answer validation, stale retries, interrupted mutation, helper validation); `wire` against `wiremock`; `CdpObserver` against the Rust fixture server + headless Chrome; parity gate (10) |
| `neo-ax` | pruning / diff / ref logic over raw trees in `fixtures/ax/`; `session` and `perm` behind traits with fakes; live `neo ax bench` manual |
| `neo-voice` | segmenter over WAV fixtures (quiet room, music, two speakers) → expected boundaries; resampler golden files; STT/TTS clients against `wiremock`; duplex gate state machine |
| `neo-judge` | request-shape snapshots; threshold logic; failure policy (timeout → fail closed); `neo judge eval fixtures/*.jsonl` live, on demand, baseline committed |
| `neo-packs` | every seeded Axoniac pack parses; hostile packs refused (bad `allowed_hosts`, `$VAR` outside `requires_env`, loosening policy); HTTP runner against `wiremock`; lock + consent diff |
| `neo-media` | fake `MediaBackend`; estimate → confirm threshold; studio index rebuild; take ids round-trip with `dmm` |
| `neo-canvas` / `-agent` | `proptest` on ops: apply∘undo = identity, per-author undo isolation, node ids stable across subtree rewrites; micro-edit op mapping; render goldens via headless Chrome |
| `neo-agent` | fake observers + stub Sol: router, queue lanes, caps, kill, lock pause, confirm broker, amend timing; recorded-session replay; spend accounting |
| `neo-cli` / end to end | `neo scenario …` on a real desktop before each release; injection scenarios must pass; `neo doctor` snapshot |
| `src-tauri` + `ui/` | bindings drift check; Vitest for store and reducers; a fixture page rendering every `TraceItem`, bar state and card from canned `AppEvent`s |

## 14. CI (GitHub Actions, `macos-latest`, Apple Silicon)

On every push and PR: `cargo fmt --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo test --workspace` (offline; live tests are `#[ignore]`) → `cargo deny check` + `cargo audit` → rule greps (§1 rules 1, 3, 4) → `neo dev bindings && git diff --exit-code` → `pnpm i --frozen-lockfile && pnpm typecheck && pnpm test` → `cargo tauri build --debug --no-bundle` unsigned. Caches: cargo registry + a per-branch target dir. **Signing, notarization, DMG and updater artifacts run only on `v*` tags**, in a separate workflow with the Apple and updater secrets scoped to a protected environment. If Actions minutes are refused for the account, the same steps run from `neo dev ci` locally and the tag workflow is dispatched by hand.

## 15. M1 build order (Shell) — day-level

| Day | Work | Green when |
|---|---|---|
| 1 | repo, workspace `Cargo.toml` (lints, profiles, shared deps), toolchain, `deny.toml`, CI skeleton; `neo-keys` + `neo-core` skeletons: ids, errors, `Settings` + defaults, `AppEvent`, `ModelRef` / `Endpoint` / `Usage`, provider traits | `fmt`, `clippy`, empty tests pass in CI |
| 2 | `neo-store`: pragmas, writer actor + read pool, `0001_init.sql` (the full schema above), settings repo + merge-patch, backup-before-migrate | migration + settings tests pass; seed DB v1 committed |
| 3 | `neo-keys` complete: `Secret`, accounts, Keychain store, `KeyValidator`, `KeySource`; `neo keys set / status / rm` | Keychain round-trip; redaction tests |
| 4 | `OpenAiInference::list_models` + validator; `jev-nav::wire` promoted from the M0 spike with `ping`; registry (classify, resolve, hide-list, `models` cache); price fetch; `neo models` | both keys validate; `sol-latest` resolves; hidden ids never listed |
| 5 | `neo-ax::perm`, `neo-voice::perm`; `neo doctor` with the §10 list (media / Chrome checks report "not yet") | doctor is correct in all four TCC states |
| 6 | Tauri scaffold: `tauri.conf.json`, `Info.plist` strings, both entitlements files, plugins (single-instance first), Developer ID wired; signed debug build into `/Applications` | **Accessibility grant and Keychain access survive three rebuilds** |
| 7 | `Runtime` facade stub in `neo-agent`; `tauri-specta` bridge: `get_bootstrap`, settings, keys, models, permissions, `AppEvent` emitter; bindings drift check in CI | UI receives typed events from Rust |
| 8 | UI shell: four Vite entries (three as stubs), store + `useAppEvents`, onboarding 1–6 against real checks, `KeyField`, `PermissionRow` | fresh user account completes onboarding |
| 9 | Settings: Keys, Models, General, Doctor; revoked-permission re-gating; updater keypair generated and backed up twice; tag workflow: sign → notarize → staple → DMG → `latest.json` | a tagged build installs on a second Mac and passes `spctl` |
| 10 | hardening: update from build N to N+1 keeps grants; diagnostics bundle; migration-from-seed test; leftovers | M1 exit: every line above still green, CI green |

Day 6 onward is blocked by open item 1 (the Developer ID). Days 1–5 are not.

## 16. Risks

| Risk | Mitigation |
|---|---|
| No signing identity yet → TCC and Keychain churn poisons the dev loop | days 1–5 need none; the CLI loop uses terminal grants + env keys; do not start M2 UI work on ad-hoc builds |
| `tauri-specta` is an rc | exact pins; the bridge is one module; `ts-rs` fallback costs about a day |
| `tauri-nspanel` is a git dependency | pin by rev; vendor if it goes stale; M0 spike proves panels over fullscreen first |
| Lock / screensaver notifications are undocumented | three independent signals, a pre-task `CGDisplayIsAsleep` check, fail-closed default |
| No vendor price API for OpenAI | signed `prices.json` until StarkRouter reports exact cost; step and wall caps always apply |
| `~/Documents` triggers a Files-and-Folders prompt at first studio creation | usage strings explain it; if the prompt is declined, studios fall back to `Application Support/…/studios/` and the UI says where |
| Losing the updater private key | two offline backups made on day 9, restore tested once |
| `metalcraft` 0.12 slips · `target/` deleted mid-build by another session | M1–M4 do not need 0.12 (M5 is the first consumer) · scratch `CARGO_TARGET_DIR` for anything longer than a check |
| SQLite growth from traces | zstd past 8 KiB, 30-day trace retention, incremental vacuum, doctor reports DB and WAL size |
