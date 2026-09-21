import type { AppEvent, KeyRow, KeyState, NoticeLevel } from "../bridge/api";

export interface NoticeRow {
  level: NoticeLevel;
  code: string;
  text: string;
  /** Unix milliseconds from the envelope. */
  at: number;
}

/**
 * What the window knows about the machine's readiness between bootstraps.
 *
 * Key *states* live here; key *values* never cross the bridge at all. A
 * `KeyStatus` event says an account changed, which is exactly enough for the
 * Connections screen to re-read itself.
 */
export interface HealthState {
  keys: Record<string, KeyState>;
  /** An account was signed in, out or rate-limited; Connections is stale. */
  accountsStale: boolean;
  /** Newest first, capped — a notice log is not a place to leak memory. */
  notices: NoticeRow[];
}

export const initialHealth: HealthState = { keys: {}, accountsStale: false, notices: [] };

const NOTICE_LIMIT = 50;

export function reduceHealth(state: HealthState, event: AppEvent, at: number): HealthState {
  switch (event.type) {
    case "key_status":
      return { ...state, keys: { ...state.keys, [event.account]: event.status } };
    case "provider_account":
      return { ...state, accountsStale: true };
    case "notice": {
      const row: NoticeRow = { level: event.level, code: event.code, text: event.text, at };
      return { ...state, notices: [row, ...state.notices].slice(0, NOTICE_LIMIT) };
    }
    default:
      return state;
  }
}

/**
 * The required key this machine has not been given, if there is one.
 *
 * Two sources, because neither is enough alone: the bootstrap rows say which
 * accounts are required — a `KeyStatus` event carries only an account and a
 * state — and the event stream says what each one is now, so a key added in
 * the Connections screen clears the shell's warning without a re-bootstrap.
 */
export function missingRequiredKey(rows: KeyRow[], state: HealthState): string | null {
  for (const row of rows) {
    const live: KeyState = state.keys[row.account] ?? row.state;
    if (row.required && live === "missing") {
      return row.account;
    }
  }
  return null;
}
