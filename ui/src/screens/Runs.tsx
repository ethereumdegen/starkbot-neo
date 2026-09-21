import { useEffect, useState } from "react";

import { NavLines, RunSteps } from "../components/RunTrace";
import { elapsedMs, type RunRecord } from "../store/runs";
import { useStore } from "../store/store";
import { tally } from "../store/trace";
import panes from "../styles/panes.module.css";
import trace from "../styles/trace.module.css";

function seconds(ms: number): string {
  return `${(ms / 1000).toFixed(1)}s`;
}

/**
 * What "progress" means depends on the run.
 *
 * A chat turn counts steps; a navigator run counts the lines it printed —
 * it has no `TurnStep` at all — and a suite counts cases. Showing "0 steps"
 * beside a navigator run that has done fifty things is worse than showing
 * nothing.
 */
function progress(record: RunRecord, navLines: number, cases: number): string {
  if (record.kind === "eval") {
    return `${cases} case${cases === 1 ? "" : "s"}`;
  }
  if (record.kind === "chat") {
    return `${record.steps.length} step${record.steps.length === 1 ? "" : "s"}`;
  }
  return `${navLines} line${navLines === 1 ? "" : "s"}`;
}

function EvalRows({ run }: { run: string }) {
  const rows = useStore((state) => state.trace.evals[run]);
  if (rows === undefined || rows.length === 0) {
    return null;
  }
  const score = tally(rows);
  return (
    <div>
      <div className={trace.stepHead}>
        {score.passed} passed · {score.failed} failed · {score.skipped} skipped · {score.total} cases
      </div>
      <table className={panes.table}>
        <thead>
          <tr>
            <th>#</th>
            <th>Case</th>
            <th>State</th>
            <th>Detail</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.case}>
              <td className={panes.num}>{row.index}</td>
              <td>{row.case}</td>
              <td>{row.state.state}</td>
              <td>
                {row.state.state === "failed"
                  ? row.state.detail
                  : row.state.state === "skipped"
                    ? row.state.reason
                    : row.state.state === "passed"
                      ? `${row.state.runs} runs`
                      : ""}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function Detail({ record, now }: { record: RunRecord; now: number }) {
  const navEntries = useStore((state) => state.trace.nav[record.run]) ?? [];
  const cases = useStore((state) => state.trace.evals[record.run])?.length ?? 0;
  const stop = useStore((state) => state.stop);
  return (
    <>
      <div className={panes.head}>
        <h2>
          {record.kind} · {record.status}
        </h2>
        <span className={`${panes.spacer} ${trace.usage}`}>
          {seconds(elapsedMs(record, now))} · {progress(record, navEntries.length, cases)}
        </span>
        {record.status === "running" && (
          <button className="danger" onClick={() => void stop(record.run)}>
            Stop
          </button>
        )}
      </div>
      <div className={panes.body}>
        <div className={trace.usage}>{record.run}</div>
        {(record.kind === "chat" || record.steps.length > 0) && <RunSteps record={record} />}
        <NavLines entries={navEntries} />
        <EvalRows run={record.run} />
        {record.text !== null && <div className={trace.answer}>{record.text}</div>}
        {record.error !== null && <div className={`${trace.answer} fail`}>{record.error}</div>}
        {record.exhausted && <div className={trace.stale}>The step budget ran out.</div>}
        {record.usage !== null && (
          <div className={trace.usage}>
            {record.usage.input_tokens} in · {record.usage.output_tokens} out ·{" "}
            {record.usage.cached_input_tokens} cached · {record.usage.requests} requests
          </div>
        )}
      </div>
    </>
  );
}

/**
 * Every run this window has heard of, including the ones it did not start:
 * the event stream is a broadcast, so a `neo nav` in a terminal shows up here
 * the moment it publishes its first step.
 */
export function Runs() {
  const order = useStore((state) => state.runs.order);
  const byId = useStore((state) => state.runs.byId);
  const selected = useStore((state) => state.ui.selectedRun);
  const selectRun = useStore((state) => state.selectRun);
  const traced = useStore((state) => state.trace);
  const [now, setNow] = useState(() => Date.now());

  const anyRunning = order.some((id) => byId[id].status === "running");
  useEffect(() => {
    if (!anyRunning) {
      return;
    }
    const timer = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(timer);
  }, [anyRunning]);

  const record = selected === null ? undefined : byId[selected];

  return (
    <div className={`${panes.columns} ${panes.split}`}>
      <section className={panes.pane} aria-label="Runs">
        <div className={panes.head}>
          <h2>Runs</h2>
        </div>
        <div className={`${panes.body} ${panes.tight}`}>
          {order.length === 0 && (
            <p className={panes.empty}>Nothing has run in this session yet.</p>
          )}
          {order.map((id) => {
            const row = byId[id];
            return (
              <button
                key={id}
                className={panes.row}
                aria-selected={id === selected}
                onClick={() => selectRun(id)}
              >
                <span className={panes.rowTitle}>
                  {row.kind} · {row.status}
                </span>
                <span className={panes.rowMeta}>
                  <span>{seconds(elapsedMs(row, now))}</span>
                  <span>
                    {progress(
                      row,
                      traced.nav[id]?.length ?? 0,
                      traced.evals[id]?.length ?? 0,
                    )}
                  </span>
                  <span>{new Date(row.startedAt).toLocaleTimeString()}</span>
                </span>
              </button>
            );
          })}
        </div>
      </section>
      <section className={panes.pane} aria-label="Trace">
        {record === undefined ? (
          <div className={panes.body}>
            <p className={panes.empty}>Pick a run to see what it did.</p>
          </div>
        ) : (
          <Detail record={record} now={now} />
        )}
      </section>
    </div>
  );
}
