import type { ActionSummary, AppEvent, RunId, RunKind, TurnUsage } from "../bridge/api";

export type RunStatus = "running" | "finished" | "failed" | "cancelled";

/**
 * One step of a run, assembled from the two events that describe it.
 *
 * `TurnStep` fires *before* the action runs and `TurnStepDone` after, which
 * is the whole point of the pair: a card can show "browsing example.com" with
 * an empty observation, and that is what makes the screen show work in flight
 * instead of a spinner. `observation === null` is therefore "still running",
 * not "produced nothing".
 */
export interface RunStep {
  step: number;
  thought: string;
  action: ActionSummary;
  observation: string | null;
  durationMs: number | null;
  notes: string[];
}

export interface RunRecord {
  run: RunId;
  kind: RunKind;
  conversation: string | null;
  status: RunStatus;
  /** Unix milliseconds, taken from the envelope rather than the wall clock. */
  startedAt: number;
  endedAt: number | null;
  steps: RunStep[];
  /**
   * The answer as it is being written, assembled from `TurnDelta`.
   *
   * This is the only assistant text there is while the turn runs: the store
   * row is flushed on a cadence with no event, and the finished row arrives
   * as one `Message` after the terminal event. Kept after the run ends too,
   * because a cancelled turn's half-answer is the thing the user stopped it
   * to read.
   */
  stream: string;
  /**
   * The highest delta `seq` applied, `-1` before the first one. A slice at
   * or below it is a replay, and appending it again would duplicate a
   * sentence in the middle of a word.
   */
  lastDelta: number;
  /** What the user said into this turn while it was running, in order. */
  steers: string[];
  /**
   * The user stopped this turn.
   *
   * A cancelled turn ends on `TurnFinished` like any other — it has an
   * answer, just a half one — so the disposition arrives separately, as the
   * note below. Without it a turn somebody stopped and a turn that finished
   * on its own would read identically.
   */
  stopped: boolean;
  /** What the run answered with, once it has. */
  text: string | null;
  error: string | null;
  /**
   * How the failure was classified, as `TurnFailed.code` carried it.
   *
   * The sentence in `error` is written for a person and gets reworded; this
   * is the name the producer gave the failure, which is what a screen may
   * branch on. `null` until a run fails.
   */
  code: string | null;
  /** The step budget ran out rather than the model answering. */
  exhausted: boolean;
  usage: TurnUsage | null;
}

export interface RunsState {
  /** Newest first — the Runs list reads top to bottom. */
  order: RunId[];
  byId: Record<RunId, RunRecord>;
}

export const initialRuns: RunsState = { order: [], byId: {} };

/**
 * How many runs the list keeps, mirroring `neo_tui::runs::RUNS_CAP`.
 *
 * This window is meant to stay open all day and every run it hears of — its
 * own, the TUI's, a `neo` invocation in a terminal — lands here, so an
 * uncapped list is a leak with a render pass attached to it. A *running*
 * run is never dropped: it is the one `Stop` has to be able to reach.
 *
 * It also bounds everything hanging off a record, `stream` included: an
 * answer is as long as one turn made it, and at most this many are held.
 */
export const RUNS_CAP = 50;

/**
 * How many step cards one run keeps, mirroring `neo_tui::state`'s
 * `TURN_CARD_CAP`. The oldest goes first: a long run's interesting end is
 * its tail, and the whole trace is in the store regardless.
 */
const TURN_CARD_CAP = 40;

/** Drop the oldest settled runs until the list is back inside `RUNS_CAP`. */
function prune(state: RunsState): RunsState {
  if (state.order.length <= RUNS_CAP) {
    return state;
  }
  const order = state.order.slice();
  const byId = { ...state.byId };
  // `order` is newest first, so the oldest candidate is at the back.
  for (let index = order.length - 1; index >= 0 && order.length > RUNS_CAP; index -= 1) {
    if (byId[order[index]].status === "running") {
      continue;
    }
    delete byId[order[index]];
    order.splice(index, 1);
  }
  return { order, byId };
}

/**
 * A run this window never started still has to be renderable.
 *
 * Progress is a broadcast, not a callback: the CLI, the TUI and this window
 * can all be watching the same run, and the one that did not start it learns
 * of it from the first event that carries the id. So every reducer path
 * creates the record it is missing instead of dropping the event.
 */
function ensure(state: RunsState, run: RunId, kind: RunKind, at: number): RunsState {
  const existing = state.byId[run];
  if (existing !== undefined) {
    return state;
  }
  const record: RunRecord = {
    run,
    kind,
    conversation: null,
    status: "running",
    startedAt: at,
    endedAt: null,
    steps: [],
    stream: "",
    lastDelta: -1,
    steers: [],
    stopped: false,
    text: null,
    error: null,
    code: null,
    exhausted: false,
    usage: null,
  };
  return prune({ order: [run, ...state.order], byId: { ...state.byId, [run]: record } });
}

function patch(state: RunsState, run: RunId, change: Partial<RunRecord>): RunsState {
  const existing = state.byId[run];
  if (existing === undefined) {
    return state;
  }
  return { order: state.order, byId: { ...state.byId, [run]: { ...existing, ...change } } };
}

function withStep(record: RunRecord, step: number, change: Partial<RunStep>): RunStep[] {
  const index = record.steps.findIndex((row) => row.step === step);
  if (index === -1) {
    const blank: RunStep = {
      step,
      thought: "",
      action: { kind: "answer", target: null, goal: null, text: null },
      observation: null,
      durationMs: null,
      notes: [],
      ...change,
    };
    const steps = [...record.steps, blank].sort((left, right) => left.step - right.step);
    return steps.length > TURN_CARD_CAP ? steps.slice(steps.length - TURN_CARD_CAP) : steps;
  }
  const steps = record.steps.slice();
  steps[index] = { ...record.steps[index], ...change };
  return steps;
}

/**
 * The note a stopped turn leaves behind, mirroring `neo_agent::agent::STOPPED`.
 *
 * `TurnFinished` carries no disposition — a turn that answered and a turn
 * the user stopped look the same on it — so the backend publishes this line
 * instead. Stable text, by contract, matched by both front ends.
 */
const STOPPED = "stopped";

/**
 * The failure codes that mean "the user stopped this", not "this broke".
 *
 * Cancellation arrives as a failure when it comes from inside a tool, and
 * colouring a deliberate stop the same red as a crashed run trains people
 * to ignore red. This used to be decided by running `/cancel/i` over the
 * sentence — a reword on either side of the bridge silently reclassified
 * every stopped run — so `TurnFailed` carries the producer's own name for
 * the failure and this reads that instead.
 *
 * Two spellings because two layers publish the event: `neo-agent` classifies
 * its executor failures and the desktop commands classify theirs (`UiError`'s
 * `CANCELLED`).
 */
const CANCELLED_CODES: Record<string, true> = { cancelled: true, agent_cancelled: true };

/**
 * `at` is the envelope's timestamp rather than `Date.now()`: the reducer has
 * to be a pure function of the event stream for the tests to mean anything,
 * and a replayed stream must produce the same durations it did live.
 *
 * `nav_step` is deliberately absent. It used to `ensure` a `running` record
 * and then discard the result when no step card was open, which is how one
 * Inspect press — `run_ax` mints a run id for a round trip that is not a
 * run — left a record nothing would ever settle: a climbing "N running"
 * badge and the 500 ms timer that badge keeps alive, for the life of the
 * window. The lines themselves are kept once, by the trace slice; a
 * navigator run this window started is named by `registerRun`, and one it
 * did not is a run whose end it would never hear about.
 */
export function reduceRuns(state: RunsState, event: AppEvent, at: number): RunsState {
  switch (event.type) {
    case "turn_started": {
      const seeded = ensure(state, event.run, "chat", at);
      return patch(seeded, event.run, { conversation: event.conversation, kind: "chat" });
    }
    case "turn_step": {
      const seeded = ensure(state, event.run, "chat", at);
      const record = seeded.byId[event.run];
      return patch(seeded, event.run, {
        steps: withStep(record, event.step, { thought: event.thought, action: event.action }),
      });
    }
    case "turn_step_done": {
      const seeded = ensure(state, event.run, "chat", at);
      const record = seeded.byId[event.run];
      return patch(seeded, event.run, {
        steps: withStep(record, event.step, {
          observation: event.observation,
          durationMs: event.duration_ms,
        }),
      });
    }
    case "turn_note": {
      const seeded = ensure(state, event.run, "chat", at);
      const record = seeded.byId[event.run];
      // The stop marker belongs to the run, not to a step: filing it as one
      // would draw a card for work that never ran.
      if (event.line === STOPPED) {
        return patch(seeded, event.run, { stopped: true });
      }
      const notes = record.steps.find((row) => row.step === event.step)?.notes ?? [];
      return patch(seeded, event.run, {
        steps: withStep(record, event.step, { notes: [...notes, event.line] }),
      });
    }
    /**
     * Appended, never reordered — the envelope stream is already in order.
     * `seq` is here for the one thing order cannot catch: a slice replayed
     * after a reconnect, which would duplicate a word mid-sentence.
     */
    case "turn_delta": {
      const seeded = ensure(state, event.run, "chat", at);
      const record = seeded.byId[event.run];
      if (event.seq <= record.lastDelta) {
        return seeded;
      }
      return patch(seeded, event.run, {
        stream: record.stream + event.text,
        lastDelta: event.seq,
      });
    }
    case "turn_steered": {
      const seeded = ensure(state, event.run, "chat", at);
      const record = seeded.byId[event.run];
      return patch(seeded, event.run, { steers: [...record.steers, event.text] });
    }
    /**
     * The running cost, into the same field the final total lands in. A
     * status line that read a live figure from one place and a final figure
     * from another would show a different number depending on which event
     * arrived last.
     */
    case "turn_cost": {
      const seeded = ensure(state, event.run, "chat", at);
      return patch(seeded, event.run, { usage: event.usage });
    }
    case "turn_finished": {
      const seeded = ensure(state, event.run, "chat", at);
      return patch(seeded, event.run, {
        status: seeded.byId[event.run].stopped ? "cancelled" : "finished",
        endedAt: at,
        text: event.text,
        exhausted: event.exhausted,
        usage: event.usage,
      });
    }
    case "turn_failed": {
      const seeded = ensure(state, event.run, "chat", at);
      return patch(seeded, event.run, {
        status: CANCELLED_CODES[event.code] === true ? "cancelled" : "failed",
        endedAt: at,
        error: event.error,
        code: event.code,
      });
    }
    case "eval_case":
      return ensure(state, event.run, "eval", at);
    default:
      return state;
  }
}

/**
 * Name a run before its first event arrives.
 *
 * `run_nav` and friends return the id immediately and the work starts behind
 * it, so without this the Runs list would show nothing until the first step —
 * and would then call an app run a nav one, since only the caller knows which
 * command it invoked.
 */
export function registerRun(
  state: RunsState,
  run: RunId,
  kind: RunKind,
  at: number,
  conversation: string | null = null,
): RunsState {
  const seeded = ensure(state, run, kind, at);
  const record = seeded.byId[run];
  return patch(seeded, run, {
    kind,
    conversation: record.conversation ?? conversation,
  });
}

/**
 * The turn a thread is in, if any: the newest chat run that named it.
 *
 * The composer and the thread both ask this — one to decide whether what was
 * typed steers the turn or starts a new one, the other to decide what to
 * paint and what the Stop button stops. Two spellings of "newest" would
 * eventually disagree, and the disagreement would be a message delivered to
 * a run nobody is watching.
 */
export function currentChatRun(state: RunsState, conversation: string | null): RunRecord | null {
  if (conversation === null) {
    return null;
  }
  for (const id of state.order) {
    const record = state.byId[id];
    if (record.kind === "chat" && record.conversation === conversation) {
      return record;
    }
  }
  return null;
}

/** How long a run has been going, or how long it took. */
export function elapsedMs(record: RunRecord, now: number): number {
  return (record.endedAt ?? now) - record.startedAt;
}
