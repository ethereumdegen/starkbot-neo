import { EVENT_GAP, type Envelope } from "../bridge/api";

/**
 * What the window knows about its own event stream.
 *
 * `seq` is monotonic and gapless per process, so a receiver that sees a jump
 * has missed something and cannot trust what it is rendering — the thread it
 * shows would be missing a message nobody will resend. The recovery is always
 * the same: read `get_bootstrap` again. `needsBootstrap` is that instruction,
 * kept in state rather than acted on inside the reducer so the reducer stays
 * pure and the test can assert the decision instead of a side effect.
 */
export interface SyncState {
  lastSeq: number | null;
  /** Set on a gap, cleared by the bootstrap that repairs it. */
  needsBootstrap: boolean;
  /** How many gaps this session has seen — the status strip says so. */
  gaps: number;
}

export const initialSync: SyncState = { lastSeq: null, needsBootstrap: false, gaps: 0 };

/**
 * Two things mean "you missed events": a `seq` that skipped, and the
 * backend's own `event_gap` notice, which it publishes when the broadcast
 * lagged. The notice carries the last `seq` it delivered rather than a new
 * one, so a front end watching only the numbers would never see it.
 */
export function reduceSync(state: SyncState, envelope: Envelope): SyncState {
  const gapNotice = envelope.event.type === "notice" && envelope.event.code === EVENT_GAP;
  const skipped = state.lastSeq !== null && envelope.seq > state.lastSeq + 1;
  const lastSeq = state.lastSeq === null ? envelope.seq : Math.max(state.lastSeq, envelope.seq);

  if (!gapNotice && !skipped) {
    return lastSeq === state.lastSeq ? state : { ...state, lastSeq };
  }
  return { lastSeq, needsBootstrap: true, gaps: state.gaps + 1 };
}
