import { useAppEvents } from "./bridge/events";
import { Automate } from "./screens/Automate";
import { Chat } from "./screens/Chat";
import { Connections } from "./screens/Connections";
import { Eval } from "./screens/Eval";
import { Inspect } from "./screens/Inspect";
import { Runs } from "./screens/Runs";
import { SettingsScreen } from "./screens/SettingsScreen";
import { missingRequiredKey } from "./store/health";
import { useStore, type Screen } from "./store/store";
import shell from "./styles/shell.module.css";

const TABS: { id: Screen; label: string }[] = [
  { id: "chat", label: "Chat" },
  { id: "runs", label: "Runs" },
  { id: "automate", label: "Automate" },
  { id: "eval", label: "Eval" },
  { id: "inspect", label: "Inspect" },
  // Its own rail entry rather than the thirteenth tab inside Settings: this
  // is where a key is typed and a subscription signed in, and a fresh
  // install cannot do anything at all until someone finds it.
  { id: "connections", label: "Connections" },
  { id: "settings", label: "Settings" },
];

function Screens({ screen }: { screen: Screen }) {
  switch (screen) {
    case "chat":
      return <Chat />;
    case "runs":
      return <Runs />;
    case "automate":
      return <Automate />;
    case "eval":
      return <Eval />;
    case "inspect":
      return <Inspect />;
    case "connections":
      return <Connections />;
    default:
      return <SettingsScreen />;
  }
}

export function App() {
  useAppEvents();

  const screen = useStore((state) => state.ui.screen);
  const setScreen = useStore((state) => state.setScreen);
  const banner = useStore((state) => state.ui.banner);
  const dismiss = useStore((state) => state.dismissBanner);
  const ready = useStore((state) => state.ui.ready);
  const catalog = useStore((state) => state.catalog);
  const running = useStore((state) =>
    state.runs.order.reduce(
      (count, id) => (state.runs.byId[id].status === "running" ? count + 1 : count),
      0,
    ),
  );
  const gaps = useStore((state) => state.sync.gaps);
  const blocked = useStore((state) => state.ui.blocked);
  /**
   * The one warning that is not dismissible.
   *
   * A required key that is not stored is not an event that happened, it is a
   * condition the machine is in: nothing can run until it is fixed, and a
   * banner the user waved away once would leave them looking at an app that
   * refuses every turn for a reason it is no longer showing. It is derived
   * from state rather than set, so storing the key clears it.
   */
  const missingKey = useStore((state) => missingRequiredKey(state.catalog.keys, state.health));

  // Before anything else: a window built against another `BRIDGE_VERSION`
  // renders the refusal and nothing else. Painting the shell over a bridge
  // neither half agrees on is how a version skew turns into six unrelated
  // bug reports.
  if (blocked !== null) {
    return (
      <div className={shell.shell} role="alert">
        <div className={shell.stale}>
          <div className={shell.staleTitle}>This window is out of date</div>
          <p>{blocked.message}</p>
          {blocked.fix?.kind === "manual" && <p className={shell.staleFix}>{blocked.fix.detail}</p>}
        </div>
      </div>
    );
  }

  return (
    <div className={shell.shell}>
      <nav className={shell.rail} aria-label="Screens">
        <div className={shell.wordmark}>Starkbot Neo</div>
        {TABS.map((tab) => (
          <button
            key={tab.id}
            className={shell.tab}
            aria-current={screen === tab.id ? "page" : undefined}
            onClick={() => setScreen(tab.id)}
          >
            <span>{tab.label}</span>
            {/* The count is a second reading of the same state the Runs
                screen shows; it is never the only one. */}
            {tab.id === "runs" && running > 0 && (
              <span className={shell.count}>{running} running</span>
            )}
          </button>
        ))}
        <div className={shell.railFoot}>
          <span>bridge v{catalog.bridgeVersion}</span>
          <span>{catalog.storePath}</span>
          {gaps > 0 && <span>{gaps} event gaps repaired</span>}
        </div>
      </nav>

      <main className={shell.main}>
        {missingKey !== null && (
          <div className={`${shell.banner} fail`} role="alert">
            <span>
              No {missingKey} key is stored, so nothing can run yet. Connections is where it
              goes.
            </span>
            <button className="link" onClick={() => setScreen("connections")}>
              Open Connections
            </button>
          </div>
        )}
        {banner !== null && (
          <div className={`${shell.banner} ${banner.tone}`} role="status">
            <span>{banner.text}</span>
            <button className="link" onClick={dismiss}>
              Dismiss
            </button>
          </div>
        )}
        <div className={shell.screen}>
          {ready ? (
            <Screens screen={screen} />
          ) : (
            <p className={shell.loading}>opening the store…</p>
          )}
        </div>
      </main>
    </div>
  );
}
