import type { AppEvent, EvalCaseState, NavStepKind, RunId } from "../bridge/api";

/**
 * One navigator line as the backend rendered it, plus the structure behind
 * it.
 *
 * Both, and not one: the string is what a log pane shows without owning a
 * formatter, and the kind is what lets the pane pick out the decisions
 * without matching on prose (events.rs says as much).
 */
export interface NavEntry {
  step: number;
  line: string;
  kind: NavStepKind;
}

export interface EvalRow {
  index: number;
  total: number;
  case: string;
  state: EvalCaseState;
}

export interface TraceState {
  nav: Record<RunId, NavEntry[]>;
  /** Per run, one row per case — a case that moves replaces its own row. */
  evals: Record<RunId, EvalRow[]>;
}

export const initialTrace: TraceState = { nav: {}, evals: {} };

export function reduceTrace(state: TraceState, event: AppEvent): TraceState {
  switch (event.type) {
    case "nav_step": {
      const entries = state.nav[event.run] ?? [];
      const entry: NavEntry = { step: event.step, line: event.line, kind: event.kind };
      return { ...state, nav: { ...state.nav, [event.run]: [...entries, entry] } };
    }
    case "eval_case": {
      const rows = state.evals[event.run] ?? [];
      const row: EvalRow = {
        index: event.index,
        total: event.total,
        case: event.case,
        state: event.state,
      };
      // A case reports twice — started, then how it ended. Appending both
      // would show every case as still running next to its own result.
      const at = rows.findIndex((existing) => existing.case === event.case);
      const next = at === -1 ? [...rows, row] : rows.slice();
      if (at !== -1) {
        next[at] = row;
      }
      return { ...state, evals: { ...state.evals, [event.run]: next } };
    }
    default:
      return state;
  }
}

export interface EvalTally {
  passed: number;
  failed: number;
  skipped: number;
  running: number;
  total: number;
}

/** The one-line score a suite header shows while the suite is still going. */
export function tally(rows: EvalRow[]): EvalTally {
  const counted: EvalTally = {
    passed: 0,
    failed: 0,
    skipped: 0,
    running: 0,
    total: rows[0]?.total ?? rows.length,
  };
  for (const row of rows) {
    if (row.state.state === "passed") {
      counted.passed += 1;
    } else if (row.state.state === "failed") {
      counted.failed += 1;
    } else if (row.state.state === "skipped") {
      counted.skipped += 1;
    } else {
      counted.running += 1;
    }
  }
  return counted;
}
