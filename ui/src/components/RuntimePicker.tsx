import type { InferenceView, ModelRow } from "../bridge/api";
import { Pill } from "./Pill";

/**
 * Which K6 connection Sol runs on. A runtime with no credential yet is shown
 * but not selectable, and says what it is waiting for — selecting it would
 * only produce `inference: fail`.
 */
export function RuntimePicker({
  inference,
  models,
  busy,
  onSelect,
  onModelSelect,
}: {
  inference: InferenceView;
  models: ModelRow[];
  busy: boolean;
  onSelect: (provider: string) => void;
  onModelSelect: (model: string) => void;
}) {
  const modelIds = Array.from(
    new Set([
      "sol-latest",
      inference.model,
      ...models
        .filter(
          (model) =>
            model.provider === inference.provider && !model.hidden && !model.deprecated,
        )
        .map((model) => model.id),
    ]),
  );
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
      <div className="model-picker">
        <div>
          <label htmlFor="inference-model">Model</label>
          <div className="meta">Choose Sol automatically or pin a model from this runtime's catalogue.</div>
        </div>
        <select
          id="inference-model"
          value={inference.model}
          disabled={busy}
          onChange={(event) => onModelSelect(event.target.value)}
        >
          {modelIds.map((model) => (
            <option key={model} value={model}>
              {model === "sol-latest" ? "Sol (latest available)" : model}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}
