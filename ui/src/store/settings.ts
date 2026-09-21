import type { AppEvent, SettingsView } from "../bridge/api";

/**
 * The settings document, exactly as Rust validated it.
 *
 * `null` before the first bootstrap, and the editor renders nothing rather
 * than defaults: a form pre-filled with guesses would let someone save a
 * value they never chose over one the store already holds.
 */
export interface SettingsState {
  settings: SettingsView | null;
  /** Bumped by every accepted change, so open editors know to re-read. */
  revision: number;
}

export const initialSettings: SettingsState = { settings: null, revision: 0 };

export function reduceSettings(state: SettingsState, event: AppEvent): SettingsState {
  if (event.type !== "settings_changed") {
    return state;
  }
  return { settings: event.settings, revision: state.revision + 1 };
}

/**
 * The model id a turn runs on, out of the untyped settings document.
 *
 * Settings cross as a record rather than 12 interfaces (foreign.ts), so the
 * path `models.inference.id` — where `neo_core::Settings` keeps the chat
 * model — has to be walked and checked here rather than read off a type.
 * `null` before the first bootstrap, and the status line says so rather than
 * naming a model nothing is running on.
 */
export function modelId(state: SettingsState): string | null {
  const inference = state.settings?.models.inference;
  if (typeof inference !== "object" || inference === null || Array.isArray(inference)) {
    return null;
  }
  return typeof inference.id === "string" ? inference.id : null;
}
