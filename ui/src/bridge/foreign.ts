// The edges of the bridge that no generator writes.
//
// `generated.ts` is produced from `src-tauri/src/view.rs` by ts-rs, and every
// view model lives there. These do not, for one of two reasons:
//
//  - ts-rs cannot express "omitted when empty": `neo-ax`'s element rows drop
//    `value`, `container` and `options` from the payload rather than sending
//    null, and a generated `options: string[]` would promise a field that is
//    not always there. The observation is therefore spelled out here, with
//    the optionality the wire actually has;
//  - TypeScript can say something Rust's type has no shape for: `Json` is
//    recursive, and `Settings` is read as the record the settings editor
//    renders rather than as 12 hand-copied interfaces.
//
// A name here that a view model refers to is not a free-floating guess: the
// generated file imports it, so deleting or renaming one breaks `tsc`.

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

/** The 12 sections of `neo_core::Settings`, in the order Settings tabs show them. */
export const SETTINGS_SECTIONS = [
  "identity",
  "listen",
  "voice",
  "models",
  "intake",
  "safety",
  "caps",
  "queue",
  "browser",
  "hotkeys",
  "general",
  "privacy",
] as const;

export type SettingsSection = (typeof SETTINGS_SECTIONS)[number];

/**
 * Settings cross as plain JSON rather than a mirrored interface per section.
 *
 * The 12 sections carry some 80 fields whose Rust definitions are the only
 * source of truth; a hand-copied mirror would be wrong the first time a field
 * is added in Rust, and wrong silently — the screen would keep rendering the
 * fields it knew and drop the new one. The editor reads the shape it is given.
 *
 * `SettingsView` in the generated file is this type: the Rust newtype is
 * transparent, so the wire carries the settings object itself.
 */
export type SettingsRecord = Record<SettingsSection, Record<string, Json>>;

export interface AxAppView {
  name: string;
  bundle_id: string | null;
  pid: number;
  frontmost: boolean;
}

export interface AxWindowView {
  title: string;
  modal: boolean;
  /** Omitted rather than null when the window holds no web area. */
  url?: string;
}

/** Checkbox / radio / toggle state; absent on controls that have none. */
export type AxChecked = "on" | "off" | "mixed";

export interface AxState {
  enabled: boolean;
  focused: boolean;
  selected: boolean;
  expanded: boolean;
  checked?: AxChecked;
}

export type AxOperation = "CLICK" | "TYPE_TEXT" | "SELECT" | "MENU";
export type AxControl = "SCROLL_UP" | "SCROLL_DOWN" | "WAIT";

/** `value`, `container` and `options` are omitted when empty, never null. */
export interface AxElementView {
  index: number;
  role: string;
  label: string;
  value?: string;
  state: AxState;
  container?: string;
  operations: AxOperation[];
  options?: string[];
}

export interface AxTableView {
  generation: number;
  app: AxAppView;
  window: AxWindowView;
  text: string;
  elements: AxElementView[];
  controls: AxControl[];
  truncated: boolean;
}
