// Generated from `src-tauri/src/view.rs` by `cargo test -p neo-desktop`.
// Do not edit: regenerate with
//
//     UPDATE_BINDINGS=1 cargo test -p neo-desktop bindings
//
// `bindings::tests::generated_bindings_are_committed` fails while this
// file differs from what the Rust view models produce, so a renamed
// field is a red test and a broken `tsc`, never a silent `undefined`.

import type { AxAppView, AxTableView, Json, SettingsRecord } from "./foreign";

/** The protocol version this bundle was built against; `handshake` refuses a core that speaks another. */
export const BRIDGE_VERSION = 3;

export type RunId = string;

export type ConversationId = string;

export type MessageId = string;

export type Health = "ok" | "warn" | "fail" | "unknown";

export type TaskId = string;

export type ProviderAccountStatus = "signed_out" | "connected" | "rate_limited" | "unavailable";

/**
 * State of one Starkbot-owned credential.
 *
 * `Missing`, `Present` and `Invalid` describe what is in the Keychain;
 * `Unchecked` and `Limited` describe what a vendor validator later learned
 * about a key that is present.
 */
export type KeyState = "missing" | "present" | "invalid" | "unchecked" | "limited";

/**
 * Where a secret came from.
 *
 * The Keychain is authoritative; the environment is a development fallback
 * only (05 §6). Reporting the source lets the UI say "this key is coming from
 * your shell, not from the Keychain" without ever naming the value.
 */
export type KeySource = "keychain" | "environment";

/**
 * Which K6 inference connection is configured and usable.
 */
export type InferenceConnection = "none" | "open_ai_key" | "chat_gpt_codex" | "anthropic_key" | "claude_subscription" | "anthropic_oauth" | "open_ai_codex_oauth";

export type MessageRole = "user" | "assistant" | "tool" | "system";

/**
 * What a message *is* in the thread, so a front end can render it without
 * re-deriving intent from the text (04 §6). One variant per value the
 * `messages.kind` CHECK accepts; the two move together, and a kind nothing
 * constructs yet lives in neither.
 */
export type MessageKind = "text" | "ask" | "result" | "answer";

export type HeartbeatGate = "hold" | "skip";

export type HeartbeatOutcome = "done" | "skipped" | "held" | "failed" | "dropped";

/**
 * One heartbeat attempt. The goal itself is deliberately absent.
 */
export type HeartbeatTick = { id: number, project: string, started_at: number, finished_at: number, outcome: HeartbeatOutcome, reason: string | null, task_id: TaskId | null, goal_bytes: number, };

/**
 * A named folder of standing work and its own heartbeat clock.
 */
export type Project = { slug: string, name: string, root: string, heartbeat_enabled: boolean, heartbeat_every_seconds: number, on_gate: HeartbeatGate, last_tick_at: number | null, next_due_at: number | null, consecutive_failures: number, created_at: number, updated_at: number, };

/**
 * Whether this binary may read and drive other applications, and where the
 * user goes to change that.
 */
export type TrustReport = { trusted: boolean, 
/**
 * Posting a synthetic key event is a separate grant from reading the
 * tree, and a run that can read but not type fails in a confusing way.
 */
can_post_events: boolean, 
/**
 * Deep link to the pane the user flips the switch in.
 */
settings: string, };

/**
 * What one hand-driven action did.
 */
export type ActReport = { performed: boolean, 
/**
 * `Debug` of [`neo_ax::Method`] — `Ax`, `Cg` — because that is what the
 * CLI has always printed and scripts parse it.
 */
method: string, 
/**
 * Whether the target had to be found again by fingerprint first.
 */
relocated: boolean, summary: string, };

export type UiError = { code: string, message: string, 
/**
 * What the user can press to get out of this, when the screen has a
 * control for it.
 */
fix?: Fix, };

/**
 * A fix the screen itself can perform. The doctor's own `fix` strings are
 * shell commands for `neo`; the desktop app never shows one — it shows the
 * control that does the job.
 */
export type Fix = { "kind": "set_key", account: string, } | { "kind": "sign_in", provider: string, } | { "kind": "choose_runtime" } | { "kind": "manual", detail: string, };

export type ConnectionRow = { provider: string, display_name: string, status: ProviderAccountStatus, email: string | null, plan: string | null, updated_at: number, 
/**
 * Is this the runtime settings currently point at?
 */
selected: boolean, };

export type LoginStart = { provider: string, 
/**
 * Public values only: client id, redirect URI, scopes, state, PKCE
 * challenge. The verifier stays in Rust.
 */
authorize_url: string, redirect_uri: string, timeout_secs: number, };

export type LoginFailed = { provider: string, error: UiError, };

export type KeyRow = { account: string, label: string, state: KeyState, 
/**
 * `keychain` or `environment` — never the value.
 */
source: KeySource | null, required: boolean, 
/**
 * Can this account's catalogue be refreshed from the vendor?
 */
refreshable: boolean, };

export type RuntimeOption = { provider: string, display_name: string, 
/**
 * `subscription` (OAuth login) or `api_key`.
 */
kind: string, 
/**
 * Is the credential this runtime needs actually there?
 */
usable: boolean, selected: boolean, };

export type InferenceView = { provider: string, model: string, connection: InferenceConnection, 
/**
 * `true` once `InferenceConnection::detect` finds a usable credential —
 * the "inference: ok" the first run is aiming at.
 */
ready: boolean, options: Array<RuntimeOption>, };

export type CheckView = { name: string, health: Health, detail: string, fix?: Fix, };

export type ModelRow = { id: string, provider: string, deprecated: boolean, hidden: boolean, };

/**
 * Every setting, verbatim.
 *
 * Transparent rather than a field-by-field copy: [`Settings`] is the
 * product's own configuration type, it is validated by the store before it
 * is ever handed out, and it holds no credential — keys live in the
 * Keychain and only a [`KeyState`] crosses this boundary. A hand-written
 * mirror would fall behind the twelve sections the moment one gained a
 * field, and a settings pane that cannot see a section cannot edit it.
 *
 * A newtype serialises as its inner value, so no `#[serde(transparent)]` is
 * needed to make `settings` the settings object itself on the wire.
 */
export type SettingsView = SettingsRecord;

/**
 * What kind of work a run is, so a Stop button can say what it stops and a
 * reloaded window can put a run back on the right screen.
 */
export type RunKind = "chat" | "nav" | "app" | "eval";

export type RunView = { run: RunId, kind: RunKind, started_at: number, };

export type ConversationView = { id: ConversationId, title: string | null, created_at: number, updated_at: number, };

/**
 * One stored line of a thread.
 *
 * `kind` travels alongside `role` because they answer different questions: a
 * `tool`/`result` row is a step card and an `assistant`/`answer` row is
 * prose, and a front end that only had the role would render an action's
 * observation as something the agent said.
 */
export type MessageView = { id: MessageId, conversation_id: ConversationId, role: MessageRole, kind: MessageKind, text: string, at: number, 
/**
 * Per-message metadata the store kept (model, tool name, token counts).
 * Never a credential: nothing writes one here, and this is persisted.
 */
meta: Json | null, };

/**
 * One eval case before anything runs.
 *
 * `installed` is the whole reason this is a view rather than the library's
 * own listing: a case whose app is missing is skipped with a reason, not run
 * and failed, and a front end that showed it as runnable would be promising
 * a measurement it cannot take.
 */
export type CaseListingView = { id: string, name: string | null, tags: Array<string>, 
/**
 * The application the case drives, by the selector `neo app` takes.
 */
app: string, installed: boolean, 
/**
 * Where the bundle was found, when it was.
 */
path: string | null, };

export type BootstrapView = { bridge_version: number, data_dir: string, store_path: string, connections: Array<ConnectionRow>, keys: Array<KeyRow>, inference: InferenceView, doctor: Array<CheckView>, 
/**
 * Every setting, as `patch_settings` takes them back section by section.
 */
settings: SettingsView, 
/**
 * The thread this window is in, and the thread switcher's rows.
 */
conversation: ConversationId, messages: Array<MessageView>, conversations: Array<ConversationView>, 
/**
 * The catalogue of the runtime inference is pointed at.
 */
models: Array<ModelRow>, eval_cases: Array<CaseListingView>, 
/**
 * Runs still going. A webview that reloaded mid-turn needs these, or it
 * shows a finished screen over work that is still driving an app.
 */
runs: Array<RunView>, projects: Array<Project>, };

export type ProjectDetailView = { project: Project, soul: string, heartbeat: string, ticks: Array<HeartbeatTick>, };

/**
 * One accessibility request, as the webview spells it.
 *
 * Tagged where [`neo_agent::ax::AxRequest`] is a plain enum, because the
 * webview has no Rust enum representation to match: `{ kind: "press", app,
 * index }` is what a TypeScript union serialises to.
 */
export type AxRequestView = { "kind": "trusted" } | { "kind": "apps" } | { "kind": "table", app: string, } | { "kind": "press", app: string, index: number, } | { "kind": "set", app: string, index: number, text: string, } | { "kind": "menu", app: string, path: string, } | { "kind": "type", app: string, text: string, } | { "kind": "key", app: string, key: string, };

/**
 * What an accessibility request answered.
 *
 * Tagged where [`AxResponse`] is untagged: the CLI's untagged shape exists so
 * scripts reading `neo ax` see bare values, and telling four object shapes
 * apart by their fields is work a renderer should not be doing. The payloads
 * themselves are `neo_ax`'s own types, passed through — a second copy of the
 * element table would drift from the one Jev is shown.
 */
export type AxResponseView = { "kind": "trust" } & TrustReport | { "kind": "apps", apps: AxAppView[], } | { "kind": "acted" } & ActReport | { "kind": "table", table: AxTableView, };

