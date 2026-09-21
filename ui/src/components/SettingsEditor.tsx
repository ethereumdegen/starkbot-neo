import { useEffect, useState } from "react";

import type { Json, SettingsSection } from "../bridge/api";
import { useStore } from "../store/store";
import panes from "../styles/panes.module.css";

type Section = Record<string, Json>;

function isObject(value: Json): value is { [key: string]: Json } {
  return typeof value === "object" && value !== null && !Array.isArray(value);
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

/**
 * One setting, rendered from the shape of its value.
 *
 * Driven by the JSON rather than by a hand-written schema: the 12 sections
 * carry some eighty fields whose definitions live in Rust, and a mirrored
 * form would quietly stop showing a field the day someone adds one. A value
 * this cannot lay out — a list, a shape it has no control for — is offered as
 * JSON rather than hidden, because hiding it would make the screen lie about
 * what the section contains.
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
  const id = path.join(".");
  const label = path[path.length - 1].replace(/_/g, " ");

  if (typeof value === "boolean") {
    return (
      <label className={panes.check}>
        <input
          id={id}
          type="checkbox"
          checked={value}
          onChange={(event) => onChange(path, event.target.checked)}
        />
        {label}
      </label>
    );
  }

  if (typeof value === "number") {
    return (
      <div className={panes.field}>
        <label htmlFor={id}>{label}</label>
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
      <div className={panes.field}>
        <label htmlFor={id}>{label}</label>
        <input id={id} value={value} onChange={(event) => onChange(path, event.target.value)} />
      </div>
    );
  }

  if (isObject(value)) {
    return (
      <fieldset className={panes.form}>
        <h3>{label}</h3>
        {Object.keys(value).map((key) => (
          <Field key={key} path={[...path, key]} value={value[key]} onChange={onChange} />
        ))}
      </fieldset>
    );
  }

  return <JsonField id={id} label={label} path={path} value={value} onChange={onChange} />;
}

function JsonField({
  id,
  label,
  path,
  value,
  onChange,
}: {
  id: string;
  label: string;
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
    <div className={panes.field}>
      <label htmlFor={id}>{label}</label>
      <textarea
        id={id}
        rows={2}
        value={text}
        aria-invalid={broken}
        onChange={(event) => {
          setText(event.target.value);
          try {
            // Parked in local state until it parses: handing the store half a
            // literal would send Rust a patch nobody typed.
            onChange(path, JSON.parse(event.target.value) as Json);
            setBroken(false);
          } catch {
            setBroken(true);
          }
        }}
      />
      {broken && <span className={`${panes.hint} fail`}>Not valid JSON yet — not saved.</span>}
    </div>
  );
}

/**
 * One settings section, edited and saved as an RFC 7386 merge patch.
 *
 * Only the top-level keys that changed are sent. Sending the whole section
 * would be a merge patch too, but it would also overwrite anything another
 * front end changed while this form sat open — and the store is shared with
 * the CLI and the TUI.
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
      className={panes.form}
      onSubmit={(event) => {
        event.preventDefault();
        const patch: Section = {};
        for (const key of changed) {
          patch[key] = draft[key];
        }
        void save(section, patch);
      }}
    >
      <h3>{section}</h3>
      {Object.keys(draft).map((key) => (
        <Field
          key={key}
          path={[key]}
          value={getIn(draft, [key])}
          onChange={(path, value) => setDraft((current) => setIn(current ?? {}, path, value))}
        />
      ))}
      <div className={panes.actions}>
        <button type="submit" className="primary" disabled={busy || changed.length === 0}>
          Save {changed.length > 0 ? `${changed.length} change${changed.length === 1 ? "" : "s"}` : ""}
        </button>
        <button type="button" disabled={changed.length === 0} onClick={() => setDraft(stored)}>
          Revert
        </button>
        <span className={panes.hint}>
          Rust validates the patch; a refused value comes back as an error and nothing is stored.
        </span>
      </div>
    </form>
  );
}
