import { useMemo, useState } from "react";

import { useStore } from "../store/store";
import { tally, type EvalRow } from "../store/trace";
import panes from "../styles/panes.module.css";
import trace from "../styles/trace.module.css";

function stateLabel(row: EvalRow | undefined): string {
  if (row === undefined) {
    return "—";
  }
  switch (row.state.state) {
    case "passed":
      return `passed (${row.state.runs} runs)`;
    case "failed":
      return `failed: ${row.state.detail}`;
    case "skipped":
      return `skipped: ${row.state.reason}`;
    default:
      return "running";
  }
}

/**
 * The suite, its cases, and what this machine can actually run.
 *
 * The run button is disabled while a suite is in flight. The backend refuses
 * a second one too — cases share the keyboard and the frontmost app, so two
 * suites would type into each other — but a disabled button explains itself
 * and a rejected command arrives as an error the user did not cause.
 */
export function Eval() {
  const cases = useStore((state) => state.catalog.evalCases);
  const runs = useStore((state) => state.runs);
  const evals = useStore((state) => state.trace.evals);
  const startEval = useStore((state) => state.startEval);
  const rescan = useStore((state) => state.rescanCases);
  const selectRun = useStore((state) => state.selectRun);
  const busy = useStore((state) => state.ui.busy);

  const [filter, setFilter] = useState("");
  const [tags, setTags] = useState("");
  const [once, setOnce] = useState(false);

  const suite = useMemo(() => {
    for (const id of runs.order) {
      if (runs.byId[id].kind === "eval") {
        return runs.byId[id];
      }
    }
    return null;
  }, [runs]);

  const running = suite !== null && suite.status === "running";
  const rows = suite === null ? [] : (evals[suite.run] ?? []);
  const byCase = new Map(rows.map((row) => [row.case, row]));
  const score = tally(rows);

  const tagList = tags
    .split(",")
    .map((tag) => tag.trim())
    .filter((tag) => tag !== "");

  const installed = cases.filter((row) => row.installed).length;

  return (
    <div className={panes.pane}>
      <div className={panes.head}>
        <h2>Eval</h2>
        <span className={`${panes.spacer} ${trace.usage}`}>
          {installed} of {cases.length} cases runnable here
        </span>
        {/* Availability is resolved per machine, so a case that was missing
            at launch becomes runnable the moment its app is installed. */}
        <button onClick={() => void rescan()} disabled={busy}>
          Rescan
        </button>
      </div>
      <div className={panes.body}>
        <form
          className={panes.form}
          onSubmit={(event) => {
            event.preventDefault();
            void startEval(filter.trim() === "" ? null : filter.trim(), tagList, once);
          }}
        >
          <div className={panes.field}>
            <label htmlFor="eval-filter">Filter</label>
            <input
              id="eval-filter"
              value={filter}
              placeholder="substring of the case id"
              onChange={(event) => setFilter(event.target.value)}
            />
          </div>
          <div className={panes.field}>
            <label htmlFor="eval-tags">Tags</label>
            <input
              id="eval-tags"
              value={tags}
              placeholder="comma separated"
              onChange={(event) => setTags(event.target.value)}
            />
          </div>
          <div className={panes.actions}>
            <label className={panes.check}>
              <input
                type="checkbox"
                checked={once}
                onChange={(event) => setOnce(event.target.checked)}
              />
              Once — one attempt per case instead of the configured repeats
            </label>
          </div>
          <div className={panes.actions}>
            <button type="submit" className="primary" disabled={busy || running}>
              {running ? "Running…" : "Run suite"}
            </button>
            {suite !== null && (
              <button type="button" onClick={() => selectRun(suite.run)}>
                Open trace
              </button>
            )}
            {rows.length > 0 && (
              <span className={trace.usage}>
                {score.passed} passed · {score.failed} failed · {score.skipped} skipped of{" "}
                {score.total}
              </span>
            )}
          </div>
          <span className={panes.hint}>
            One suite at a time: the cases drive the real keyboard and the frontmost app.
          </span>
        </form>

        <table className={panes.table}>
          <thead>
            <tr>
              <th>Case</th>
              <th>App</th>
              <th>Tags</th>
              <th>Installed</th>
              <th>Result</th>
            </tr>
          </thead>
          <tbody>
            {cases.map((row) => (
              <tr key={row.id}>
                <td>{row.name ?? row.id}</td>
                <td>{row.app}</td>
                <td>{row.tags.join(", ")}</td>
                <td className={row.installed ? "ok" : "warn"}>
                  {row.installed ? "installed" : "missing"}
                </td>
                <td>{stateLabel(byCase.get(row.id) ?? byCase.get(row.name ?? row.id))}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {cases.length === 0 && <p className={panes.empty}>The suite listed no cases.</p>}
      </div>
    </div>
  );
}
