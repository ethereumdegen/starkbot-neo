import { useEffect, useState } from "react";

import { SETTINGS_SECTIONS, type SettingsSection } from "../bridge/api";
import { SettingsEditor } from "../components/SettingsEditor";
import { useStore } from "../store/store";
import panes from "../styles/panes.module.css";

/**
 * The 12 settings sections.
 *
 * Connections is not among them any more: it is where a key is typed and a
 * subscription signed in, and a fresh install that cannot find it cannot do
 * anything at all. It has its own entry in the rail, and exactly one — two
 * ways in would mean two places to look when one of them is wrong.
 */
export function SettingsScreen() {
  const [tab, setTab] = useState<SettingsSection>("identity");
  const reload = useStore((state) => state.reloadSettings);

  // The bootstrap already carried settings; this re-reads them on the way in
  // so a window left open while the CLI edited the store is not showing a
  // form built from stale values.
  useEffect(() => {
    void reload();
  }, [reload]);

  return (
    <div className={panes.pane}>
      <div className={panes.tabs} role="tablist" aria-label="Settings sections">
        {SETTINGS_SECTIONS.map((section) => (
          <button
            key={section}
            role="tab"
            aria-current={tab === section ? "page" : undefined}
            onClick={() => setTab(section)}
          >
            {section}
          </button>
        ))}
      </div>
      <div className={panes.body}>
        <SettingsEditor section={tab} />
      </div>
    </div>
  );
}
