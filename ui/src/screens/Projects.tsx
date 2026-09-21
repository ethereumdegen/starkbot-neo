import { useEffect, useState } from "react";

import {
  api,
  errorOf,
  type HeartbeatGate,
  type Project,
  type ProjectDetailView,
} from "../bridge/api";
import panes from "../styles/panes.module.css";

function when(value: number | null): string {
  return value === null ? "never" : new Date(value).toLocaleString();
}

type ProjectSection = "settings" | "activity" | "soul" | "heartbeat";

export function Projects() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [detail, setDetail] = useState<ProjectDetailView | null>(null);
  const [soul, setSoul] = useState("");
  const [heartbeat, setHeartbeat] = useState("");
  const [everySeconds, setEverySeconds] = useState("14400");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [root, setRoot] = useState("");
  const [section, setSection] = useState<ProjectSection>("settings");

  const applyDetail = (next: ProjectDetailView) => {
    setDetail(next);
    setSoul(next.soul);
    setHeartbeat(next.heartbeat);
    setEverySeconds(String(next.project.heartbeat_every_seconds));
    setProjects((current) => {
      const exists = current.some((project) => project.slug === next.project.slug);
      return exists
        ? current.map((project) => project.slug === next.project.slug ? next.project : project)
        : [next.project, ...current];
    });
  };

  const run = async (work: () => Promise<ProjectDetailView>): Promise<boolean> => {
    setBusy(true);
    setError(null);
    try {
      applyDetail(await work());
      return true;
    } catch (thrown) {
      setError(errorOf(thrown).message);
      return false;
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    let live = true;
    void api.listProjects().then((rows) => {
      if (live) setProjects(rows);
    }).catch((thrown) => {
      if (live) setError(errorOf(thrown).message);
    });
    return () => { live = false; };
  }, []);

  const open = async (project: Project) => {
    setBusy(true);
    setError(null);
    try {
      setSection("settings");
      applyDetail(await api.showProject(project.slug));
    } catch (thrown) {
      setError(errorOf(thrown).message);
    } finally {
      setBusy(false);
    }
  };

  const create = async () => {
    if (!(await run(() => api.createProject(name.trim(), root)))) {
      return;
    }
    setName("");
    setRoot("");
    setSection("settings");
    setCreating(false);
  };
  return (
    <div className={`${panes.columns} ${panes.split}`}>
      <section className={panes.pane}>
        <div className={panes.head}>
          {detail === null ? (
            <>
              <h2>Projects</h2>
              <button className={panes.spacer} onClick={() => setCreating((value) => !value)}>
                {creating ? "Cancel" : "New project"}
              </button>
            </>
          ) : (
            <>
              <button
                onClick={() => {
                  setDetail(null);
                  setSection("settings");
                }}
              >
                Back
              </button>
              <h2>{detail.project.name}</h2>
            </>
          )}
        </div>
        <div className={`${panes.body} ${panes.tight}`}>
          {detail === null ? (
            <>
              {creating && (
                <form
                  className={panes.form}
                  onSubmit={(event) => {
                    event.preventDefault();
                    void create();
                  }}
                >
                  <h3>New project</h3>
                  <div className={panes.field}>
                    <label htmlFor="project-name">Name</label>
                    <input
                      id="project-name"
                      autoFocus
                      value={name}
                      placeholder="Q4 Launch"
                      onChange={(event) => setName(event.target.value)}
                    />
                  </div>
                  <div className={panes.field}>
                    <label htmlFor="project-root">Existing folder</label>
                    <input
                      id="project-root"
                      value={root}
                      placeholder="optional — managed when blank"
                      onChange={(event) => setRoot(event.target.value)}
                    />
                  </div>
                  <span className={panes.hint}>
                    A blank folder uses Starkbot's data directory. An existing folder is never created or scanned.
                  </span>
                  <button type="submit" className="primary" disabled={busy || name.trim() === ""}>
                    Create project
                  </button>
                </form>
              )}
              {projects.length === 0 && !creating && (
                <p className={panes.empty}>No projects yet. Create one here to get started.</p>
              )}
              {projects.map((project) => (
                <button
                  key={project.slug}
                  className={panes.row}
                  onClick={() => { void open(project); }}
                >
                  <span className={panes.rowTitle}>{project.name}</span>
                  <span className={panes.rowMeta}>
                    <span>
                      {project.heartbeat_enabled
                        ? `every ${project.heartbeat_every_seconds}s`
                        : "heartbeat off"}
                    </span>
                    <span>last {when(project.last_tick_at)}</span>
                  </span>
                </button>
              ))}
            </>
          ) : (
            <>
              <div className={panes.projectNavSection}>
                <div className={panes.projectNavLabel}>Project</div>
                <div className={panes.projectPills}>
                  <button
                    aria-current={section === "settings" ? "page" : undefined}
                    onClick={() => setSection("settings")}
                  >
                    Project Settings
                  </button>
                  <button
                    aria-current={section === "activity" ? "page" : undefined}
                    onClick={() => setSection("activity")}
                  >
                    Activity
                  </button>
                </div>
              </div>
              <div className={panes.projectNavSection}>
                <div className={panes.projectNavLabel}>Files</div>
                <div className={panes.projectFiles}>
                  <button
                    className={panes.row}
                    aria-selected={section === "soul"}
                    onClick={() => setSection("soul")}
                  >
                    <span className={panes.rowTitle}>soul.md</span>
                    <span className={panes.rowMeta}>standing context</span>
                  </button>
                  <button
                    className={panes.row}
                    aria-selected={section === "heartbeat"}
                    onClick={() => setSection("heartbeat")}
                  >
                    <span className={panes.rowTitle}>heartbeat.md</span>
                    <span className={panes.rowMeta}>recurring task</span>
                  </button>
                </div>
              </div>
            </>
          )}
        </div>
      </section>

      <section className={panes.pane}>
        <div className={panes.head}>
          <h2>
            {detail === null
              ? "Project"
              : section === "settings"
                ? "Project Settings"
                : section === "activity"
                  ? "Activity"
                  : section === "soul"
                    ? "soul.md"
                    : "heartbeat.md"}
          </h2>
          {detail !== null && (
            <button
              className={`${panes.spacer} primary`}
              disabled={busy}
              onClick={() => { void run(() => api.runProjectHeartbeat(detail.project.slug)); }}
            >
              Run heartbeat
            </button>
          )}
        </div>
        <div className={panes.body}>
          {error !== null && <p className="fail" role="alert">{error}</p>}
          {detail === null && (
            <p className={panes.empty}>Select a project to open its settings, files, and activity.</p>
          )}
          {detail !== null && section === "settings" && (
            <div className={panes.form}>
              <h3>Clock</h3>
              <p className={panes.hint}>{detail.project.root}</p>
              <label className={panes.check}>
                <input
                  type="checkbox"
                  checked={detail.project.heartbeat_enabled}
                  disabled={busy}
                  onChange={(event) => {
                    const enabled = event.target.checked;
                    void run(() => api.configureProjectHeartbeat(
                      detail.project.slug,
                      enabled,
                      detail.project.heartbeat_every_seconds,
                      detail.project.on_gate,
                    ));
                  }}
                />
                Run automatically
              </label>
              <div className={panes.field}>
                <label htmlFor="project-interval">Every (seconds)</label>
                <input
                  id="project-interval"
                  type="number"
                  min={300}
                  max={604800}
                  value={everySeconds}
                  onChange={(event) => setEverySeconds(event.target.value)}
                />
              </div>
              <div className={panes.field}>
                <label htmlFor="project-on-gate">When a gate appears</label>
                <select
                  id="project-on-gate"
                  value={detail.project.on_gate}
                  disabled={busy}
                  onChange={(event) => {
                    const gate = event.target.value as HeartbeatGate;
                    void run(() => api.configureProjectHeartbeat(
                      detail.project.slug,
                      detail.project.heartbeat_enabled,
                      detail.project.heartbeat_every_seconds,
                      gate,
                    ));
                  }}
                >
                  <option value="hold">Hold for me</option>
                  <option value="skip">Skip the gated action</option>
                </select>
              </div>
              <div className={panes.actions}>
                <button
                  disabled={
                    busy
                    || Number(everySeconds) < 300
                    || Number(everySeconds) > 604800
                  }
                  onClick={() => {
                    void run(() => api.configureProjectHeartbeat(
                      detail.project.slug,
                      detail.project.heartbeat_enabled,
                      Number(everySeconds),
                      detail.project.on_gate,
                    ));
                  }}
                >
                  Apply clock
                </button>
              </div>
              <p className={panes.hint}>Next {when(detail.project.next_due_at)}</p>
            </div>
          )}
          {detail !== null && section === "soul" && (
            <div className={panes.form}>
              <h3>soul.md</h3>
              <textarea rows={16} value={soul} onChange={(event) => setSoul(event.target.value)} />
              <div className={panes.actions}>
                <button disabled={busy} onClick={() => {
                  void run(() => api.saveProjectDocument(detail.project.slug, "soul.md", soul));
                }}>Save soul</button>
              </div>
            </div>
          )}
          {detail !== null && section === "heartbeat" && (
            <div className={panes.form}>
              <h3>heartbeat.md</h3>
              <textarea
                rows={16}
                value={heartbeat}
                onChange={(event) => setHeartbeat(event.target.value)}
              />
              <div className={panes.actions}>
                <button disabled={busy} onClick={() => {
                  void run(() => api.saveProjectDocument(
                    detail.project.slug,
                    "heartbeat.md",
                    heartbeat,
                  ));
                }}>Save heartbeat</button>
              </div>
            </div>
          )}
          {detail !== null && section === "activity" && (
            <div className={panes.form}>
              <h3>Recent ticks</h3>
              {detail.ticks.length === 0 ? <p className={panes.empty}>No ticks yet.</p> : (
                <table className={panes.table}>
                  <thead><tr><th>Started</th><th>Outcome</th><th>Reason</th></tr></thead>
                  <tbody>{detail.ticks.map((tick) => (
                    <tr key={tick.id}>
                      <td>{when(tick.started_at)}</td>
                      <td>{tick.outcome}</td>
                      <td>{tick.reason ?? ""}</td>
                    </tr>
                  ))}</tbody>
                </table>
              )}
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
