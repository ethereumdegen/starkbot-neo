import type { BootstrapView, Envelope } from "../bridge/api";
import { initialConversation, reduceConversation, type ConversationState } from "./conversation";
import { initialGates, reduceGates, type GatesState } from "./gates";
import { initialHealth, reduceHealth, type HealthState } from "./health";
import { initialRuns, reduceRuns, registerRun, type RunsState } from "./runs";
import { initialSettings, reduceSettings, type SettingsState } from "./settings";
import { initialSync, reduceSync, type SyncState } from "./sync";
import { initialTrace, reduceTrace, type TraceState } from "./trace";

/**
 * Everything the window derives from the event stream, and nothing it
 * derives from the DOM.
 *
 * 04 §15 asks for this shape specifically: slices reduced by pure functions,
 * so the interesting behaviour — a turn assembling itself out of six events,
 * a `seq` gap demanding a re-bootstrap — is testable without a webview, a
 * Tauri runtime or a running agent.
 */
export interface AppState {
  sync: SyncState;
  conversation: ConversationState;
  runs: RunsState;
  trace: TraceState;
  settings: SettingsState;
  health: HealthState;
  gates: GatesState;
}

export const initialState: AppState = {
  sync: initialSync,
  conversation: initialConversation,
  runs: initialRuns,
  trace: initialTrace,
  settings: initialSettings,
  health: initialHealth,
  gates: initialGates,
};

/**
 * Apply one envelope to the whole state.
 *
 * The timestamp is taken from the envelope and handed to the slices that need
 * a clock, so a state is a pure function of the stream: the same events in
 * the same order always produce the same durations, which is what lets the
 * tests assert on them.
 */
export function applyEnvelope(state: AppState, envelope: Envelope): AppState {
  const at = Date.parse(envelope.at);
  const clock = Number.isNaN(at) ? 0 : at;
  const { event } = envelope;

  const next: AppState = {
    sync: reduceSync(state.sync, envelope),
    conversation: reduceConversation(state.conversation, event),
    runs: reduceRuns(state.runs, event, clock),
    trace: reduceTrace(state.trace, event),
    settings: reduceSettings(state.settings, event),
    health: reduceHealth(state.health, event, clock),
    gates: reduceGates(state.gates, event),
  };

  const changed = (Object.keys(next) as (keyof AppState)[]).some((key) => next[key] !== state[key]);
  return changed ? next : state;
}

/**
 * Re-seat the state on a fresh `get_bootstrap`.
 *
 * This is the repair for a gap, so it replaces rather than merges: keeping
 * anything derived from events the window may have missed is precisely the
 * mistake the gap is telling it to undo. Runs are the one carry-over — the
 * backend lists the ones still live, and their steps are not in the bootstrap,
 * so a run already in flight keeps the trace it has and gains a Stop button.
 */
export function applyBootstrap(state: AppState, boot: BootstrapView): AppState {
  let runs = state.runs;
  for (const live of boot.runs) {
    runs = registerRun(runs, live.run, live.kind, live.started_at);
  }
  return {
    sync: { ...state.sync, needsBootstrap: false },
    conversation: {
      conversations: boot.conversations,
      activeId: boot.conversation,
      messages: boot.messages,
      threadStale: false,
      listStale: false,
    },
    runs,
    trace: state.trace,
    settings: { settings: boot.settings, revision: state.settings.revision + 1 },
    // Cards are not in the bootstrap and must not be dropped by one: a run
    // blocked on a confirm is still blocked while this window repairs a gap,
    // and clearing the card would hide the only control that unblocks it.
    gates: state.gates,
    health: { ...state.health, accountsStale: false },
  };
}
