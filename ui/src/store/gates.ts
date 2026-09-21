import type { AppEvent, AskId, AskView, ConfirmId, ConfirmView } from "../bridge/api";

/**
 * The questions a run is blocked on, and nothing else.
 *
 * A card is not a notification: while one is up, a run is stopped waiting for
 * it, so the slice keeps them in arrival order and drops each one the moment
 * the run says it is resolved — whoever resolved it. The TUI, the pill and a
 * spoken answer all reach the same broker, so this window learns that its
 * card is gone the same way it learns anything else: from the stream.
 */
export interface GatesState {
  /** Oldest first: the run that has been waiting longest is answered first. */
  confirms: ConfirmView[];
  asks: AskView[];
  /**
   * Cards this window has sent an answer for and not yet seen resolved.
   *
   * The card stays on screen until the resolve arrives — the answer has not
   * landed until the run says so — but it is inert, so a second press cannot
   * send a second answer into the gap.
   */
  pending: Record<string, true>;
  /**
   * Ids resolved before their request was seen, newest first and capped.
   *
   * The two events are published by different code paths — a run that times
   * a card out publishes the resolve while the request is still crossing the
   * IPC boundary — and a window that ignored the early resolve would raise a
   * card nothing is waiting on and leave it up forever.
   */
  settled: string[];
}

export const initialGates: GatesState = { confirms: [], asks: [], pending: {}, settled: [] };

/**
 * How many out-of-order resolves are remembered. A card that arrives more
 * than this many resolves after its own is not a race, and an unbounded list
 * would be a leak in the one slice that is never cleared by a bootstrap.
 */
const SETTLED_LIMIT = 64;

function without(pending: Record<string, true>, id: string): Record<string, true> {
  if (pending[id] !== true) {
    return pending;
  }
  const next = { ...pending };
  delete next[id];
  return next;
}

export function reduceGates(state: GatesState, event: AppEvent): GatesState {
  switch (event.type) {
    case "confirm_request": {
      const { confirm } = event;
      const early = state.settled.indexOf(confirm.id);
      if (early !== -1) {
        // Already answered before we heard it was asked. Forget the id
        // rather than keep it: it can never be asked again.
        const settled = state.settled.slice();
        settled.splice(early, 1);
        return { ...state, settled };
      }
      // Republished rather than new — a re-bootstrapped window can see the
      // same card twice, and two cards for one waiting run would let the
      // user answer the second after the first had already won.
      if (state.confirms.some((row) => row.id === confirm.id)) {
        return state;
      }
      return { ...state, confirms: [...state.confirms, confirm] };
    }
    case "confirm_resolved": {
      const id = event.confirm_id;
      const confirms = state.confirms.filter((row) => row.id !== id);
      if (confirms.length === state.confirms.length) {
        // Resolved before its request was seen; remembered so the request
        // cannot raise a card nothing is waiting on. Newest first, capped.
        const settled = [id, ...state.settled.filter((seen) => seen !== id)];
        return { ...state, settled: settled.slice(0, SETTLED_LIMIT) };
      }
      return { ...state, confirms, pending: without(state.pending, id) };
    }
    case "ask_request": {
      const { ask } = event;
      const early = state.settled.indexOf(ask.id);
      if (early !== -1) {
        const settled = state.settled.slice();
        settled.splice(early, 1);
        return { ...state, settled };
      }
      if (state.asks.some((row) => row.id === ask.id)) {
        return state;
      }
      return { ...state, asks: [...state.asks, ask] };
    }
    case "ask_resolved": {
      const id = event.ask_id;
      const asks = state.asks.filter((row) => row.id !== id);
      if (asks.length === state.asks.length) {
        const settled = [id, ...state.settled.filter((seen) => seen !== id)];
        return { ...state, settled: settled.slice(0, SETTLED_LIMIT) };
      }
      return { ...state, asks, pending: without(state.pending, id) };
    }
    default:
      return state;
  }
}

/**
 * Claim a card for the answer this window is about to send.
 *
 * `null` means somebody already pressed: the store makes no second call, so
 * a double click, an Enter held down or a second component rendering the same
 * card cannot answer it twice. The claim is released by the resolve event, or
 * by [`releaseGate`] when the command itself was refused.
 */
export function claimGate(state: GatesState, id: ConfirmId | AskId): GatesState | null {
  if (state.pending[id] === true) {
    return null;
  }
  return { ...state, pending: { ...state.pending, [id]: true } };
}

/** The command was refused, so the card is answerable again. */
export function releaseGate(state: GatesState, id: ConfirmId | AskId): GatesState {
  const pending = without(state.pending, id);
  return pending === state.pending ? state : { ...state, pending };
}

/**
 * The card this window is showing, if any.
 *
 * One at a time, oldest first: two cards side by side ask the user to answer
 * questions from two runs at once, and the one they read is the one that
 * happened to render on top. Confirms come before asks because a confirm is
 * holding an action back, and an ask is holding a question open.
 */
export function frontGate(
  state: GatesState,
): { kind: "confirm"; confirm: ConfirmView } | { kind: "ask"; ask: AskView } | null {
  const confirm = state.confirms[0];
  if (confirm !== undefined) {
    return { kind: "confirm", confirm };
  }
  const ask = state.asks[0];
  return ask === undefined ? null : { kind: "ask", ask };
}
