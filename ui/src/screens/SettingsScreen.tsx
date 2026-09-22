import { useEffect, useState } from "react";

import { SETTINGS_SECTIONS, type SettingsSection } from "../bridge/api";
import { SettingsEditor } from "../components/SettingsEditor";
import { useStore } from "../store/store";
import panes from "../styles/panes.module.css";

const SECTION_COPY: Record<SettingsSection, { title: string; description: string }> = {
  identity: { title: "Identity", description: "Name, personality, and how Stark presents itself." },
  listen: { title: "Listening", description: "Wake behavior, microphone timing, and speech intake." },
  voice: { title: "Voice", description: "Spoken replies, voice selection, and duplex behavior." },
  models: { title: "Models", description: "Inference, helper, speech, and reasoning model choices." },
  intake: { title: "Intake", description: "Confidence thresholds for accepting and offering work." },
  safety: { title: "Safety", description: "Confirmation rules and boundaries for sensitive actions." },
  caps: { title: "Limits", description: "Per-task, daily, step, action, and time budgets." },
  queue: { title: "Queue", description: "Ordering, concurrency, and unattended task behavior." },
  browser: { title: "Browser", description: "Managed Chrome profile and web automation behavior." },
  hotkeys: { title: "Hotkeys", description: "Keyboard shortcuts for fast control and capture." },
  general: { title: "General", description: "Startup, updates, appearance, and default behavior." },
  privacy: { title: "Privacy", description: "Retention, recordings, telemetry, and local data." },
};

export function SettingsScreen() {
  const [tab, setTab] = useState<SettingsSection>("identity");
  const reload = useStore((state) => state.reloadSettings);
  const active = SECTION_COPY[tab];

  useEffect(() => {
    void reload();
  }, [reload]);

  return (
    <div className={`${panes.columns} ${panes.settingsLayout}`}>
      <aside className={panes.settingsNav}>
        <header className={panes.settingsNavHead}>
          <span>Preferences</span>
          <h1>Settings</h1>
          <p>Make Stark work the way you do.</p>
        </header>
        <nav role="tablist" aria-label="Settings categories">
          {SETTINGS_SECTIONS.map((section) => {
            const copy = SECTION_COPY[section];
            return (
              <button
                key={section}
                role="tab"
                aria-selected={tab === section}
                className={panes.settingsNavItem}
                onClick={() => setTab(section)}
              >
                <span>{copy.title}</span>
                <small>{copy.description}</small>
              </button>
            );
          })}
        </nav>
      </aside>
      <section className={`${panes.pane} ${panes.settingsContent}`} role="tabpanel">
        <header className={panes.settingsHeader}>
          <span>Settings / {active.title}</span>
          <h2>{active.title}</h2>
          <p>{active.description}</p>
        </header>
        <div className={`${panes.body} ${panes.settingsBody}`}>
          <SettingsEditor section={tab} />
        </div>
      </section>
    </div>
  );
}
