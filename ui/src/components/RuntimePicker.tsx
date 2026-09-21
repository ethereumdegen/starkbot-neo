import type { InferenceView } from "../bridge/api";
import { Pill } from "./Pill";

/**
 * Which K6 connection Sol runs on. A runtime with no credential yet is shown
 * but not selectable, and says what it is waiting for — selecting it would
 * only produce `inference: fail`.
 */
export function RuntimePicker({
  inference,
  busy,
  onSelect,
}: {
  inference: InferenceView;
  busy: boolean;
  onSelect: (provider: string) => void;
}) {
  return (
    <div className="runtimes">
      {inference.options.map((option) => (
        <div className={`runtime${option.selected ? " selected" : ""}`} key={option.provider}>
          <div>
            <div>
              {option.display_name}
              {option.selected ? ` · model ${inference.model}` : ""}
            </div>
            <div className="meta mono">
              {option.provider} ·{" "}
              {option.usable
                ? "credential ready"
                : option.kind === "subscription"
                  ? "sign in above first"
                  : "add the key below first"}
            </div>
          </div>
          {option.selected ? (
            <Pill tone={inference.ready ? "ok" : "fail"} label={inference.ready ? "In use" : "Selected, not usable"} />
          ) : (
            <button disabled={busy || !option.usable} onClick={() => onSelect(option.provider)}>
              Use this
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
