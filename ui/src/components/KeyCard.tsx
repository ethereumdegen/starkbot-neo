import { useState } from "react";

import type { KeyRow, KeyState } from "../bridge/api";
import { Pill, type Tone } from "./Pill";

const STATE: Record<KeyState, { tone: Tone; label: string }> = {
  present: { tone: "ok", label: "Present" },
  limited: { tone: "warn", label: "Limited" },
  invalid: { tone: "fail", label: "Rejected" },
  unchecked: { tone: "unknown", label: "Unchecked" },
  missing: { tone: "unknown", label: "Missing" },
};

/**
 * One Keychain account. The field is masked, is cleared the moment the value
 * is handed to Rust, and is never filled from anything Rust returns — the
 * bridge only ever answers with a state.
 */
export function KeyCard({
  row,
  busy,
  focusRef,
  modelCount,
  onSave,
  onCheck,
  onRemove,
  onRefreshModels,
}: {
  row: KeyRow;
  busy: boolean;
  focusRef?: (element: HTMLInputElement | null) => void;
  modelCount?: number;
  onSave: (value: string) => Promise<void>;
  onCheck: () => void;
  onRemove: () => void;
  onRefreshModels: () => void;
}) {
  const [value, setValue] = useState("");
  const state = STATE[row.state];
  const stored = row.state !== "missing";
  const missingRequired = row.required && !stored;

  return (
    <div className={`key-row${missingRequired ? " required-missing" : ""}`}>
      <div>
        <div>
          {row.label}
          {row.required ? " · required" : ""}
        </div>
        <div className="meta mono">
          {row.account}
          {row.source ? ` · ${row.source}` : ""}
          {modelCount !== undefined ? ` · ${modelCount} models cached` : ""}
        </div>
      </div>
      <div className="paste">
        <input
          type="password"
          ref={focusRef}
          value={value}
          autoComplete="off"
          spellCheck={false}
          placeholder={stored ? "Replace the stored key" : "Paste the key"}
          aria-label={`${row.label} value`}
          onChange={(event) => setValue(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && value.trim() !== "") {
              const pasted = value;
              setValue("");
              void onSave(pasted);
            }
          }}
        />
        <button
          disabled={busy || value.trim() === ""}
          onClick={() => {
            const pasted = value;
            setValue("");
            void onSave(pasted);
          }}
        >
          Save
        </button>
      </div>
      <div className="actions">
        <Pill tone={state.tone} label={state.label} />
        {stored && (
          <button onClick={onCheck} disabled={busy}>
            Check
          </button>
        )}
        {stored && row.refreshable && (
          <button onClick={onRefreshModels} disabled={busy}>
            Refresh models
          </button>
        )}
        {stored && (
          <button className="danger" onClick={onRemove} disabled={busy}>
            Remove
          </button>
        )}
      </div>
    </div>
  );
}
