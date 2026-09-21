import type { ActionSummary } from "../bridge/api";
import type { RunRecord, RunStep } from "../store/runs";
import type { NavEntry } from "../store/trace";
import styles from "../styles/trace.module.css";

/**
 * The CLI's one-line form of an action, rebuilt here.
 *
 * `ActionSummary` is sent structured rather than pre-rendered so a consumer
 * can tell a URL from a goal (events.rs); this is the one place the front end
 * flattens it again, and it matches `impl Display for ActionSummary` so the
 * desktop app and the terminal say the same words about the same step.
 */
export function actionLine(action: ActionSummary): string {
  switch (action.kind) {
    case "browse":
    case "app":
      return `${action.kind === "browse" ? "browse" : "app"} ${action.target ?? "?"} — ${
        action.goal ?? "?"
      }`;
    case "answer":
      return "answer";
    default:
      return "ask the user";
  }
}

function StepCard({ step }: { step: RunStep }) {
  const running = step.observation === null;
  return (
    <li className={running ? `${styles.step} ${styles.running}` : styles.step}>
      <div className={styles.stepHead}>
        <span className={styles.index}>step {step.step}</span>
        <span>{running ? "running" : `${step.durationMs ?? 0} ms`}</span>
      </div>
      {step.thought !== "" && <div className={styles.thought}>{step.thought}</div>}
      <div className={styles.action}>{actionLine(step.action)}</div>
      {step.notes.length > 0 && (
        <ul className={styles.notes}>
          {step.notes.map((note, index) => (
            <li key={`${step.step}-${index}`}>{note}</li>
          ))}
        </ul>
      )}
      {/* What the navigator did inside this action, while it is doing it:
          the card is the only place these lines belong, and a step that is
          still running is the one they arrived under. */}
      {step.nav.length > 0 && <NavLines entries={step.nav} />}
      {running ? (
        <div className={styles.pending} aria-live="polite">
          working…
        </div>
      ) : (
        <div className={styles.observation}>{step.observation}</div>
      )}
    </li>
  );
}

/** Thought → action → observation, one card per step, oldest first. */
export function RunSteps({ record }: { record: RunRecord }) {
  if (record.steps.length === 0) {
    return <p className={styles.thought}>No step yet — the model is deciding what to do.</p>;
  }
  return (
    <ol className={styles.steps}>
      {record.steps.map((step) => (
        <StepCard key={step.step} step={step} />
      ))}
    </ol>
  );
}

/**
 * Navigator lines, rendered from the string the backend sent.
 *
 * Re-deriving the layout from `NavDecision` would be a second formatter to
 * keep in step with the terminal's; the structured half is used for what a
 * string cannot do — telling a decision from an outcome, and flagging a
 * decision the surface moved under.
 */
export function NavLines({ entries }: { entries: NavEntry[] }) {
  if (entries.length === 0) {
    return null;
  }
  return (
    <div className={styles.lines}>
      {entries.map((entry, index) => {
        const stale = entry.kind.kind === "decision" && entry.kind.stale;
        const tone = stale ? styles.stale : styles[entry.kind.kind];
        return (
          <span key={`${entry.step}-${index}`} className={tone}>
            {entry.line}
          </span>
        );
      })}
    </div>
  );
}
