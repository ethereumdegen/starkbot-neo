// The Rust ↔ TS bridge.
//
// 04 §14 asks for generated types and names the fallback if the
// `tauri-specta` rc bites: "`ts-rs` for types + one hand-written
// `bridge/api.ts` with the same names". The rc is still not in the lockfile,
// so this is that fallback, with the types no longer hand-written:
// `generated.ts` is written from `src-tauri/src/view.rs` by
// `cargo test -p neo-desktop` and re-exported here, `foreign.ts` holds the
// few edges ts-rs cannot reach, and what is left below is the part no
// generator can infer — the command names, which are the ones in
// `src-tauri/src/commands.rs`, and the event types, which mirror
// `neo_core::events` because the backend forwards an `Envelope` verbatim.
//
// Every window starts with `handshake`: a bundle built against another
// `BRIDGE_VERSION` is refused by name, because `ui/dist` outlives the binary
// it was built for and the alternative is discovering the mismatch as an
// `undefined` three screens in.
//
// Nothing secret crosses this boundary. A key value goes one way only: from
// the masked field into `set_key`, never back.
//
// This module is the only place `invoke` is called, and `events.ts` the only
// place `listen` is: a component that reached for either would be a second
// path into the store, and the first one to drift would be the one nobody
// re-bootstraps.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { Json, SettingsSection } from "./foreign";
import {
  BRIDGE_VERSION,
  type AxRequestView,
  type AxResponseView,
  type BootstrapView,
  type CaseListingView,
  type CheckView,
  type ConnectionRow,
  type ConversationId,
  type ConversationView,
  type InferenceView,
  type KeyRow,
  type KeyState,
  type LoginFailed,
  type LoginStart,
  type MessageView,
  type ModelRow,
  type RunId,
  type SettingsView,
  type UiError,
} from "./generated";

// One import for every consumer: `from "../bridge/api"` reaches the generated
// contract and its hand-written edges alike. A star rather than a list, so a
// type added in Rust arrives here without a second list to remember.
export * from "./foreign";
export * from "./generated";

// ---------------------------------------------------------------------------
// Events. `app://event` carries `neo_core::Envelope` verbatim: the backend does
// not re-wrap it, so these types mirror `crates/neo-core/src/events.rs`.
// ---------------------------------------------------------------------------

export type ActionKind = "browse" | "app" | "answer" | "ask";

export interface ActionSummary {
  kind: ActionKind;
  target: string | null;
  goal: string | null;
  text: string | null;
}

export interface TurnUsage {
  input_tokens: number;
  output_tokens: number;
  cached_input_tokens: number;
  requests: number;
}

export interface NavDecision {
  surface: "browser" | "app";
  operation: string;
  label: string | null;
  operation_confidence: number;
  target_confidence: number | null;
  candidates: number;
  stale: boolean;
  typed_chars: number | null;
  observe_ms: number;
  jev_ms: number;
  text_ms: number;
  act_ms: number;
  elapsed_ms: number;
  safety: [string, number][];
}

/**
 * Internally tagged on `kind`, and `Decision` is a newtype over
 * `NavDecision` — so a decision arrives with the decision's fields beside
 * the tag, not nested under one.
 */
export type NavStepKind =
  | { kind: "launch" }
  | ({ kind: "decision" } & NavDecision)
  | { kind: "outcome" }
  | { kind: "summary" };

export type EvalCaseState =
  | { state: "started" }
  | { state: "passed"; runs: number }
  | { state: "failed"; runs: number; detail: string }
  | { state: "skipped"; reason: string };

export type NoticeLevel = "info" | "warning" | "error";

/**
 * `neo_core::ConfirmId` and `AskId` are `#[serde(transparent)]` newtypes over
 * a Uuid, exactly as `RunId` is — they cross as strings.
 */
export type ConfirmId = string;
export type AskId = string;
export type TaskId = string;

/** Which surface answered a card. A press in this window is always `card`. */
export type ResolutionVia = "card" | "thread" | "pill" | "voice" | "timeout";

/**
 * How a confirm ended. The window only ever *sends* the first two: a timeout
 * is the broker's arithmetic, and a cancellation is the run giving up on its
 * own question.
 */
export type GateOutcome = "confirmed" | "denied" | "timed_out" | "cancelled";

/** `neo_core::Usd`: internally tagged on `kind`, with the amount in `usd`. */
export type Usd =
  | { kind: "exact"; usd: number }
  | { kind: "estimated"; usd: number }
  | { kind: "unpriced" };

export interface TokenCounts {
  input: number;
  output: number;
  cached_input: number;
  reasoning: number;
}

export interface Units {
  tokens: TokenCounts;
  audio_seconds: number;
  characters: number;
  images: number;
  video_seconds: number;
}

export interface Usage {
  usd: Usd;
  units: Units;
}

/**
 * A question the run is blocked on: it will not act until this is answered.
 *
 * `action_sentence` is a whole sentence in the product's voice — the backend
 * writes it, the card shows it, and a front end that recomposed it from
 * `cause` would say something the other front end does not. `cause` is the
 * machine-ish tag behind it (`safety:spends`, `label:pay`) and `context` is
 * "<page title> — <url>" when there is a page involved.
 */
export interface ConfirmView {
  id: ConfirmId;
  task_id: TaskId;
  cause: string;
  action_sentence: string;
  context: string | null;
  estimated_cost: Usage | null;
  can_remember: boolean;
  /** Unix milliseconds. The broker enforces it; the card only shows it. */
  expires_at: number;
}

/**
 * Something the run needs told. `options` empty means free text — an unknown
 * field value — and a non-empty list means the answer is one of its strings,
 * which is how a sign-in hand-over arrives as a single "I'm ready".
 */
export interface AskView {
  id: AskId;
  task_id: TaskId;
  question: string;
  options: string[];
  voice_window_ends: number | null;
}

/**
 * Only the variants a front end in this batch acts on are spelled out.
 *
 * The wire carries more than these — `listen_state`, `task_upserted`, `ring`
 * and the rest of `neo_core::AppEvent`. They are not in the
 * union on purpose: a catch-all `{ type: string }` member would poison every
 * `switch` narrowing, since `string` overlaps every literal tag. An unmodelled
 * variant arrives at runtime, matches no `case`, and falls into the `default`
 * that returns the state unchanged — so a new Rust variant never breaks this
 * build, it is simply not rendered until someone models it.
 */
export type AppEvent =
  | { type: "message"; message: MessageView }
  | { type: "conversation_reset"; conversation_id: ConversationId }
  | { type: "turn_started"; run: RunId; conversation: ConversationId }
  | { type: "turn_step"; run: RunId; step: number; thought: string; action: ActionSummary }
  | { type: "turn_step_done"; run: RunId; step: number; observation: string; duration_ms: number }
  | { type: "turn_note"; run: RunId; step: number; line: string }
  // Assistant text as the model produces it. `seq` orders the slices within
  // one run; a front end appends them, it never reorders. The finished row
  // is published once, as a `message`, after the turn's terminal event — so
  // while a run is live these are the only assistant text there is.
  | { type: "turn_delta"; run: RunId; seq: number; text: string }
  // Something the user said into a turn that was already running.
  | { type: "turn_steered"; run: RunId; text: string }
  // The running token cost, republished after every model round trip, so a
  // status line can show what a turn is costing while it is still costing
  // it. `TurnFinished.usage` carries the same shape and the last word.
  | { type: "turn_cost"; run: RunId; usage: TurnUsage }
  | {
      type: "turn_finished";
      run: RunId;
      text: string;
      steps: number;
      exhausted: boolean;
      usage: TurnUsage | null;
    }
  // `error` is a sentence for a person and gets reworded; `code` is the
  // producer's own stable name for the failure — `neo_agent`'s `error_code`
  // or the desktop's `UiError::code` — so a screen can tell a deliberate
  // stop from a crash without matching on prose.
  | { type: "turn_failed"; run: RunId; error: string; code: string }
  | { type: "nav_step"; run: RunId; step: number; line: string; kind: NavStepKind }
  // A run is blocked on a question. The card stays up until the matching
  // `*_resolved` arrives — which it always does, because the run publishes
  // it whoever answered and however it ended, including on a timeout.
  | { type: "confirm_request"; confirm: ConfirmView }
  | { type: "confirm_resolved"; confirm_id: ConfirmId; outcome: GateOutcome; via: ResolutionVia }
  | { type: "ask_request"; ask: AskView }
  | { type: "ask_resolved"; ask_id: AskId; answer: string; via: ResolutionVia }
  | {
      type: "eval_case";
      run: RunId;
      index: number;
      total: number;
      case: string;
      state: EvalCaseState;
    }
  | { type: "settings_changed"; settings: SettingsView }
  | { type: "key_status"; account: string; status: KeyState }
  | { type: "provider_account"; account: Json }
  | { type: "notice"; level: NoticeLevel; code: string; text: string };

export interface Envelope {
  seq: number;
  /** RFC 3339, from `time::serde::rfc3339`. */
  at: string;
  event: AppEvent;
}

/** The one event name the backend publishes on (04 §14). */
export const APP_EVENT = "app://event";

/**
 * The backend's own gap marker: on a lagged broadcast it publishes a notice
 * with this code rather than letting `seq` silently skip, because a webview
 * that reconnected never saw the seq it is missing.
 */
export const EVENT_GAP = "event_gap";

export const api = {
  /**
   * The first call a window makes.
   *
   * `ui/dist` is a build artefact that outlives the binary it was built for,
   * so the webview is the half of this app that can skew. It sends the
   * version it was generated against and the core rejects anything else with
   * a `bridge_version` `UiError` — one sentence and the command that repairs
   * it, instead of an `undefined` three screens in.
   */
  handshake: () => invoke<number>("handshake", { uiVersion: BRIDGE_VERSION }),

  getBootstrap: () => invoke<BootstrapView>("get_bootstrap"),

  // Chat.
  sendMessage: (conversation: ConversationId, text: string) =>
    invoke<RunId>("send_message", { conversation, text }),
  stopRun: (run: RunId) => invoke<boolean>("stop_run", { run }),
  /**
   * Say something into a turn that is already running.
   *
   * `false` is not a failure: the run ended between the keystroke and the
   * command, and the message should be sent as a new turn instead.
   */
  steerRun: (run: RunId, text: string) => invoke<boolean>("steer_run", { run, text }),
  newConversation: (title?: string) =>
    invoke<ConversationView>("new_conversation", { title: title ?? null }),
  renameConversation: (id: ConversationId, title: string) =>
    invoke<null>("rename_conversation", { id, title }),
  listConversations: (limit: number) =>
    invoke<ConversationView[]>("list_conversations", { limit }),
  loadThread: (id: ConversationId, limit: number) =>
    invoke<MessageView[]>("load_thread", { id, limit }),

  // Cards. `via` is fixed at `card` rather than taken from the caller: a
  // press in this window *is* a card press, and a front end that could claim
  // otherwise would put a lie in the run's own record of how it was answered.
  // Both commands reject with `no_such_card` when something else answered
  // first — the other front end, the user's voice, or the clock.
  resolveConfirm: (id: ConfirmId, outcome: "confirmed" | "denied") =>
    invoke<null>("resolve_confirm", { id, outcome, via: "card" }),
  answerAsk: (id: AskId, answer: string) =>
    invoke<null>("answer_ask", { id, answer, via: "card" }),

  // Automate.
  runNav: (
    url: string,
    goal: string,
    headless: boolean,
    profile: string | null,
    attach: string[],
    safety: boolean,
  ) => invoke<RunId>("run_nav", { url, goal, headless, profile, attach, safety }),
  runAppGoal: (app: string, goal: string) => invoke<RunId>("run_app_goal", { app, goal }),

  // Inspect.
  runAx: (request: AxRequestView) => invoke<AxResponseView>("run_ax", { request }),

  // Eval.
  listEvalCases: () => invoke<CaseListingView[]>("list_eval_cases"),
  runEval: (filter: string | null, tags: string[], once: boolean) =>
    invoke<RunId>("run_eval", { filter, tags, once }),

  // Settings.
  getSettings: () => invoke<SettingsView>("get_settings"),
  patchSettings: (section: SettingsSection, value: Json) =>
    invoke<SettingsView>("patch_settings", { section, value }),

  // Connections (unchanged since the first screen).
  connections: () => invoke<ConnectionRow[]>("connections"),
  beginLogin: (provider: string) => invoke<LoginStart>("begin_login", { provider }),
  openLoginPage: (provider: string) => invoke<null>("open_login_page", { provider }),
  finishLoginPasted: (provider: string, pasted: string) =>
    invoke<ConnectionRow>("finish_login_pasted", { provider, pasted }),
  cancelLogin: (provider: string) => invoke<boolean>("cancel_login", { provider }),
  disconnect: (provider: string) => invoke<ConnectionRow>("disconnect", { provider }),
  runDoctor: () => invoke<CheckView[]>("run_doctor"),
  keyStatus: () => invoke<KeyRow[]>("key_status"),
  setKey: (account: string, value: string) => invoke<KeyRow>("set_key", { account, value }),
  checkKey: (account: string) => invoke<KeyRow>("check_key", { account }),
  removeKey: (account: string) => invoke<KeyRow>("remove_key", { account }),
  setInferenceRuntime: (provider: string, model?: string) =>
    invoke<InferenceView>("set_inference_runtime", { provider, model: model ?? null }),
  /** Omitting `provider` lists the models of whichever runtime is selected. */
  listModels: (provider?: string) => invoke<ModelRow[]>("list_models", { provider: provider ?? null }),
  refreshModels: (account: string) => invoke<ModelRow[]>("refresh_models", { account }),
};

export function onAppEvent(handler: (envelope: Envelope) => void): Promise<UnlistenFn> {
  return listen<Envelope>(APP_EVENT, (event) => handler(event.payload));
}

export function onLoginDone(handler: (row: ConnectionRow) => void): Promise<UnlistenFn> {
  return listen<ConnectionRow>("login:done", (event) => handler(event.payload));
}

export function onLoginFailed(handler: (failure: LoginFailed) => void): Promise<UnlistenFn> {
  return listen<LoginFailed>("login:failed", (event) => handler(event.payload));
}

/** Every command rejects with a `UiError`; anything else is a bug worth showing. */
export function errorOf(thrown: unknown): UiError {
  if (thrown && typeof thrown === "object" && "message" in thrown && "code" in thrown) {
    return thrown as UiError;
  }
  return { code: "bridge", message: String(thrown) };
}
