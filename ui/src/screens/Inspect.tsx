import { useState } from "react";

import type { AxRequestView, AxResponseView } from "../bridge/api";
import { useStore } from "../store/store";
import panes from "../styles/panes.module.css";
import trace from "../styles/trace.module.css";

type Kind = AxRequestView["kind"];

const KINDS: Kind[] = ["trusted", "apps", "table", "press", "set", "menu", "type", "key"];

/** Which fields each request variant actually carries. */
const NEEDS_APP: Record<Kind, boolean> = {
  trusted: false,
  apps: false,
  table: true,
  press: true,
  set: true,
  menu: true,
  type: true,
  key: true,
};

function Response({ response }: { response: AxResponseView }) {
  switch (response.kind) {
    case "trust":
      return (
        <div className={panes.form}>
          <h3>Accessibility</h3>
          <div className={response.trusted ? "ok" : "fail"}>
            {response.trusted ? "trusted" : "not trusted"} ·{" "}
            {response.can_post_events ? "may post events" : "may not post events"}
          </div>
          {/* Posting a synthetic key is a separate grant from reading the
              tree, and a run that can read but not type fails confusingly. */}
          <div className={panes.hint}>{response.settings}</div>
        </div>
      );
    case "apps":
      return (
        <table className={panes.table}>
          <thead>
            <tr>
              <th>App</th>
              <th>Bundle</th>
              <th>PID</th>
              <th>Frontmost</th>
            </tr>
          </thead>
          <tbody>
            {response.apps.map((app) => (
              <tr key={app.pid}>
                <td>{app.name}</td>
                <td>{app.bundle_id ?? "—"}</td>
                <td className={panes.num}>{app.pid}</td>
                <td>{app.frontmost ? "yes" : ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
      );
    case "acted":
      return (
        <div className={panes.form}>
          <h3>{response.performed ? "Done" : "Refused"}</h3>
          <div>{response.summary}</div>
          <div className={panes.hint}>
            via {response.method}
            {response.relocated ? " · target was found again by fingerprint" : ""}
          </div>
        </div>
      );
    default: {
      const { table } = response;
      return (
        <>
          <div className={trace.usage}>
            {table.app.name} · {table.window.title === "" ? "untitled window" : table.window.title}
            {table.window.modal ? " · modal" : ""} · generation {table.generation}
            {table.truncated ? " · truncated to fit" : ""}
          </div>
          <table className={panes.table}>
            <thead>
              <tr>
                <th>#</th>
                <th>Role</th>
                <th>Label</th>
                <th>Value</th>
                <th>State</th>
                <th>Operations</th>
              </tr>
            </thead>
            <tbody>
              {table.elements.map((element) => (
                <tr key={element.index}>
                  <td className={panes.num}>{element.index}</td>
                  <td>{element.role}</td>
                  <td>{element.label}</td>
                  <td>{element.value ?? ""}</td>
                  <td>
                    {[
                      element.state.enabled ? "" : "disabled",
                      element.state.focused ? "focused" : "",
                      element.state.selected ? "selected" : "",
                      element.state.expanded ? "expanded" : "",
                      element.state.checked ?? "",
                    ]
                      .filter((word) => word !== "")
                      .join(" ")}
                  </td>
                  <td>{element.operations.join(" ")}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {table.controls.length > 0 && (
            <div className={panes.hint}>controls: {table.controls.join(" ")}</div>
          )}
        </>
      );
    }
  }
}

/**
 * The accessibility tree, by hand.
 *
 * This is the surface `neo ax` prints, and the reason it is in the desktop
 * app: when a navigator run picks the wrong row, the question is always what
 * the tree looked like at that moment, and reading it needs the same indices
 * the model was given.
 */
export function Inspect() {
  const inspect = useStore((state) => state.inspect);
  const busy = useStore((state) => state.ui.busy);

  const [kind, setKind] = useState<Kind>("trusted");
  const [app, setApp] = useState("");
  const [index, setIndex] = useState("0");
  const [text, setText] = useState("");
  const [response, setResponse] = useState<AxResponseView | null>(null);

  const build = (): AxRequestView => {
    switch (kind) {
      case "trusted":
        return { kind: "trusted" };
      case "apps":
        return { kind: "apps" };
      case "table":
        return { kind: "table", app };
      case "press":
        return { kind: "press", app, index: Number(index) };
      case "set":
        return { kind: "set", app, index: Number(index), text };
      case "menu":
        return { kind: "menu", app, path: text };
      case "type":
        return { kind: "type", app, text };
      default:
        return { kind: "key", app, key: text };
    }
  };

  const textLabel =
    kind === "menu" ? "Menu path" : kind === "key" ? "Key name" : "Text";

  return (
    <div className={panes.pane}>
      <div className={panes.head}>
        <h2>Inspect</h2>
      </div>
      <div className={panes.body}>
        <form
          className={panes.form}
          onSubmit={(event) => {
            event.preventDefault();
            void inspect(build()).then((answer) => {
              if (answer !== null) {
                setResponse(answer);
              }
            });
          }}
        >
          <div className={panes.field}>
            <label htmlFor="ax-kind">Request</label>
            <select
              id="ax-kind"
              value={kind}
              onChange={(event) => setKind(event.target.value as Kind)}
            >
              {KINDS.map((option) => (
                <option key={option} value={option}>
                  {option}
                </option>
              ))}
            </select>
          </div>
          {NEEDS_APP[kind] && (
            <div className={panes.field}>
              <label htmlFor="ax-app">Application</label>
              <input
                id="ax-app"
                value={app}
                placeholder="Mail, or frontmost"
                onChange={(event) => setApp(event.target.value)}
              />
            </div>
          )}
          {(kind === "press" || kind === "set") && (
            <div className={panes.field}>
              <label htmlFor="ax-index">Row</label>
              <input
                id="ax-index"
                type="number"
                min={0}
                value={index}
                onChange={(event) => setIndex(event.target.value)}
              />
            </div>
          )}
          {(kind === "set" || kind === "menu" || kind === "type" || kind === "key") && (
            <div className={panes.field}>
              <label htmlFor="ax-text">{textLabel}</label>
              <input
                id="ax-text"
                value={text}
                placeholder={kind === "menu" ? "File › Export…" : ""}
                onChange={(event) => setText(event.target.value)}
              />
            </div>
          )}
          <div className={panes.actions}>
            <button
              type="submit"
              className="primary"
              disabled={busy || (NEEDS_APP[kind] && app.trim() === "")}
            >
              Ask
            </button>
            <span className={panes.hint}>
              Indices belong to one observation: act on a row and the table may already have moved.
            </span>
          </div>
        </form>
        {response !== null && <Response response={response} />}
      </div>
    </div>
  );
}
