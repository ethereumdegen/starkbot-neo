import { useState } from "react";

import { useStore } from "../store/store";
import panes from "../styles/panes.module.css";

/**
 * The two hand-driven capabilities, as forms.
 *
 * Both start a run and jump to its trace rather than reporting "started":
 * the interesting thing about a navigator run is what it decides, and a
 * screen that only said it had begun would send the user looking for it.
 */
export function Automate() {
  const startNav = useStore((state) => state.startNav);
  const startApp = useStore((state) => state.startApp);
  const busy = useStore((state) => state.ui.busy);

  const [url, setUrl] = useState("");
  const [navGoal, setNavGoal] = useState("");
  const [headed, setHeaded] = useState(true);
  const [profile, setProfile] = useState("");
  const [attach, setAttach] = useState("");
  const [safety, setSafety] = useState(true);

  const [app, setApp] = useState("");
  const [appGoal, setAppGoal] = useState("");

  return (
    <div className={panes.pane}>
      <div className={panes.head}>
        <h2>Automate</h2>
      </div>
      <div className={panes.body}>
        <form
          className={panes.form}
          onSubmit={(event) => {
            event.preventDefault();
            void startNav({
              url: url.trim(),
              goal: navGoal.trim(),
              headed,
              profile,
              attach: attach
                .split("\n")
                .map((line) => line.trim())
                .filter((line) => line !== ""),
              safety,
            });
          }}
        >
          <h3>Browse</h3>
          <div className={panes.field}>
            <label htmlFor="nav-url">URL</label>
            <input
              id="nav-url"
              value={url}
              placeholder="https://example.com/pricing"
              onChange={(event) => setUrl(event.target.value)}
            />
          </div>
          <div className={panes.field}>
            <label htmlFor="nav-goal">Goal</label>
            <input
              id="nav-goal"
              value={navGoal}
              placeholder="read the per-seat price"
              onChange={(event) => setNavGoal(event.target.value)}
            />
          </div>
          <div className={panes.field}>
            <label htmlFor="nav-profile">Chrome profile</label>
            <input
              id="nav-profile"
              value={profile}
              placeholder="default"
              onChange={(event) => setProfile(event.target.value)}
            />
          </div>
          <div className={panes.field}>
            <label htmlFor="nav-attach">Attach</label>
            <textarea
              id="nav-attach"
              rows={2}
              value={attach}
              placeholder="one file path per line"
              onChange={(event) => setAttach(event.target.value)}
            />
          </div>
          <div className={panes.actions}>
            <label className={panes.check}>
              <input
                type="checkbox"
                checked={headed}
                onChange={(event) => setHeaded(event.target.checked)}
              />
              Headed — show the window
            </label>
            <label className={panes.check}>
              <input
                type="checkbox"
                checked={safety}
                onChange={(event) => setSafety(event.target.checked)}
              />
              Safety heads
            </label>
          </div>
          <div className={panes.actions}>
            <button
              type="submit"
              className="primary"
              disabled={busy || url.trim() === "" || navGoal.trim() === ""}
            >
              Run
            </button>
            <span className={panes.hint}>
              Stopping a run cancels its token; a Chrome window it already opened stays open.
            </span>
          </div>
        </form>

        <form
          className={panes.form}
          onSubmit={(event) => {
            event.preventDefault();
            void startApp(app.trim(), appGoal.trim());
          }}
        >
          <h3>Drive an app</h3>
          <div className={panes.field}>
            <label htmlFor="app-name">Application</label>
            <input
              id="app-name"
              value={app}
              placeholder="Mail"
              onChange={(event) => setApp(event.target.value)}
            />
          </div>
          <div className={panes.field}>
            <label htmlFor="app-goal">Goal</label>
            <input
              id="app-goal"
              value={appGoal}
              placeholder="reply to the newest message"
              onChange={(event) => setAppGoal(event.target.value)}
            />
          </div>
          <div className={panes.actions}>
            <button
              type="submit"
              className="primary"
              disabled={busy || app.trim() === "" || appGoal.trim() === ""}
            >
              Run
            </button>
            <span className={panes.hint}>
              The run takes the keyboard and the frontmost app while it works.
            </span>
          </div>
        </form>
      </div>
    </div>
  );
}
