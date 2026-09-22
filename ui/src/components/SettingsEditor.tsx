import { useEffect, useState } from "react";

import type { Json, SettingsSection } from "../bridge/api";
import { useStore } from "../store/store";
import panes from "../styles/panes.module.css";

type Section = Record<string, Json>;

function isObject(value: Json): value is { [key: string]: Json } {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const LABEL_WORDS: Record<string, string> = {
  api: "API",
  id: "ID",
  ms: "ms",
  stt: "STT",
  tts: "TTS",
  ui: "UI",
  url: "URL",
  usd: "USD",
};

function labelOf(key: string): string {
  return key
    .split("_")
    .map((word) => LABEL_WORDS[word] ?? `${word.charAt(0).toUpperCase()}${word.slice(1)}`)
    .join(" ");
}

/** Replace one leaf of a nested object without mutating what React is holding. */
function setIn(target: Section, path: string[], value: Json): Section {
  const [head, ...rest] = path;
  if (rest.length === 0) {
    return { ...target, [head]: value };
  }
  const child = target[head];
  const nested = isObject(child) ? child : {};
  return { ...target, [head]: setIn(nested, rest, value) };
}

function getIn(target: Section, path: string[]): Json {
  let cursor: Json = target;
  for (const key of path) {
    if (!isObject(cursor)) {
      return null;
    }
    cursor = cursor[key];
  }
  return cursor;
}

function FieldLabel({ id, path }: { id: string; path: string[] }) {
  return (
    <div className={panes.settingsFieldCopy}>
      <label htmlFor={id}>{labelOf(path[path.length - 1])}</label>
      <code>{path.join(".")}</code>
    </div>
  );
}

/**
 * One setting, rendered from the shape of its value.
 *
 * Driven by the JSON rather than by a hand-written schema: Rust remains the
 * source of truth, and a newly added setting appears here automatically.
 */
function Field({
  path,
  value,
  onChange,
}: {
  path: string[];
  value: Json;
  onChange: (path: string[], value: Json) => void;
}) {
  const id = `setting-${path.join("-")}`;

  if (typeof value === "boolean") {
    return (
      <label className={panes.settingsToggleRow} htmlFor={id}>
        <span className={panes.settingsFieldCopy}>
          <span>{labelOf(path[path.length - 1])}</span>
          <code>{path.join(".")}</code>
        </span>
        <span className={panes.settingsToggle}>
          <input
            id={id}
            type="checkbox"
            checked={value}
            onChange={(event) => onChange(path, event.target.checked)}
          />
          <span aria-hidden="true" />
        </span>
      </label>
    );
  }

  if (typeof value === "number") {
    return (
      <div className={panes.settingsField}>
        <FieldLabel id={id} path={path} />
        <input
          id={id}
          type="number"
          step="any"
          value={value}
          onChange={(event) => {
            const parsed = Number(event.target.value);
            onChange(path, Number.isNaN(parsed) ? value : parsed);
          }}
        />
      </div>
    );
  }

  if (typeof value === "string") {
    return (
      <div className={panes.settingsField}>
        <FieldLabel id={id} path={path} />
        <input id={id} value={value} onChange={(event) => onChange(path, event.target.value)} />
      </div>
    );
  }

  if (isObject(value)) {
    return (
      <section className={panes.settingsGroup}>
        <header className={panes.settingsGroupHead}>
          <div>
            <h3>{labelOf(path[path.length - 1])}</h3>
            <code>{path.join(".")}</code>
          </div>
          <span>{Object.keys(value).length} fields</span>
        </header>
        <div className={panes.settingsFields}>
          {Object.keys(value).map((key) => (
            <Field key={key} path={[...path, key]} value={value[key]} onChange={onChange} />
          ))}
        </div>
      </section>
    );
  }

  return (
    <JsonField
      id={id}
      path={path}
      value={value}
      onChange={onChange}
    />
  );
}

function JsonField({
  id,
  path,
  value,
  onChange,
}: {
  id: string;
  path: string[];
  value: Json;
  onChange: (path: string[], value: Json) => void;
}) {
  const [text, setText] = useState(() => JSON.stringify(value));
  const [broken, setBroken] = useState(false);

  useEffect(() => {
    setText(JSON.stringify(value));
    setBroken(false);
  }, [value]);

  return (
    <div className={panes.settingsField}>
      <FieldLabel id={id} path={path} />
      <div>
        <textarea
          id={id}
          rows={3}
          value={text}
          aria-invalid={broken}
          onChange={(event) => {
            setText(event.target.value);
            try {
              onChange(path, JSON.parse(event.target.value) as Json);
              setBroken(false);
            } catch {
              setBroken(true);
            }
          }}
        />
        {broken && <span className={`${panes.settingsError} fail`}>Enter valid JSON before saving.</span>}
      </div>
    </div>
  );
}

/**
 * One settings section, edited and saved as an RFC 7386 merge patch.
 *
 * Only changed top-level keys are sent, so a CLI or TUI edit made while this
 * form is open is not overwritten by an unrelated save.
 */
export function SettingsEditor({ section }: { section: SettingsSection }) {
  const settings = useStore((state) => state.settings.settings);
  const revision = useStore((state) => state.settings.revision);
  const save = useStore((state) => state.saveSettings);
  const busy = useStore((state) => state.ui.busy);

  const stored = settings?.[section] ?? null;
  const [draft, setDraft] = useState<Section | null>(stored);

  useEffect(() => {
    setDraft(settings?.[section] ?? null);
  }, [settings, section, revision]);

  if (draft === null || stored === null) {
    return <p className={panes.empty}>Settings have not loaded yet.</p>;
  }

  const changed = Object.keys(draft).filter(
    (key) => JSON.stringify(draft[key]) !== JSON.stringify(stored[key]),
  );

  return (
    <form
      className={panes.settingsForm}
      onSubmit={(event) => {
        event.preventDefault();
        const patch: Section = {};
        for (const key of changed) {
          patch[key] = draft[key];
        }
        void save(section, patch);
      }}
    >
      <div className={panes.settingsGroups}>
        {Object.keys(draft).map((key) => (
          <Field
            key={key}
            path={[key]}
            value={getIn(draft, [key])}
            onChange={(path, value) => setDraft((current) => setIn(current ?? {}, path, value))}
          />
        ))}
      </div>
      <div className={panes.settingsActions}>
        <div>
          <strong>
            {changed.length === 0
              ? "Everything is saved"
              : `${changed.length} unsaved change${changed.length === 1 ? "" : "s"}`}
          </strong>
          <span>Values are validated by Starkbot before they are stored.</span>
        </div>
        <button type="button" disabled={changed.length === 0 || busy} onClick={() => setDraft(stored)}>
          Revert
        </button>
        <button type="submit" className="primary" disabled={busy || changed.length === 0}>
          {busy ? "Saving…" : "Save changes"}
        </button>
      </div>
    </form>
  );
}
