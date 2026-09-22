import { useEffect, useState } from "react";

import { api, errorOf } from "../bridge/api";
import panes from "../styles/panes.module.css";

/**
 * The exact Spice Lab shipped by `neo-eval`, inside Starkbot's own shell.
 *
 * The loopback server lives in this desktop process and receives the same
 * `Arc<Runtime>` as Connections and Chat. This iframe therefore preserves the
 * standalone lab 1:1 without maintaining a second renderer for its report and
 * trace schema.
 */
export function Eval() {
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    void api.evalUiUrl()
      .then((next) => {
        if (active) setUrl(next);
      })
      .catch((thrown) => {
        if (active) setError(errorOf(thrown).message);
      });
    return () => {
      active = false;
    };
  }, []);

  if (error !== null) {
    return <div className={panes.evalError}>The built-in Spice Lab could not open: {error}</div>;
  }
  if (url === null) {
    return <div className={panes.evalLoading}>Opening Spice Lab…</div>;
  }
  return (
    <div className={panes.evalEmbed}>
      <iframe className={panes.evalFrame} src={url} title="Starkbot Spice Lab" />
    </div>
  );
}
