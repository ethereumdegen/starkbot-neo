import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { flushSync } from "react-dom";

import { useAppEvents } from "./bridge/events";
import { api, errorOf, onWindowModeRequest, type WindowMode, type WindowModeRequest } from "./bridge/api";
import { setMiniMode } from "./bridge/window";
import { AgentComposer, type InputMode } from "./components/AgentComposer";
import { MiniMode } from "./components/MiniMode";
import { useDictation } from "./hooks/useDictation";
import { currentChatRun } from "./store/runs";
import { Chat } from "./screens/Chat";
import { Connections } from "./screens/Connections";
import { Inspect } from "./screens/Inspect";
import { Projects } from "./screens/Projects";
import { Runs } from "./screens/Runs";
import { SettingsScreen } from "./screens/SettingsScreen";
import { missingRequiredKey } from "./store/health";
import { useStore, type Screen } from "./store/store";
import shell from "./styles/shell.module.css";

const TABS: { id: Screen; label: string }[] = [
  { id: "chat", label: "Chat" },
  { id: "runs", label: "Runs" },
  { id: "projects", label: "Projects" },
  { id: "inspect", label: "Inspect" },
  // Its own rail entry rather than the thirteenth tab inside Settings: this
  // is where a key is typed and a subscription signed in, and a fresh
  // install cannot do anything at all until someone finds it.
  { id: "connections", label: "Connections" },
  { id: "settings", label: "Settings" },
];

function Screens({ screen, projectsEpoch, composer }: {
  screen: Screen;
  projectsEpoch: number;
  composer: ReactNode;
}) {
  switch (screen) {
    case "chat":
      return <Chat composer={composer} />;
    case "runs":
      return <Runs />;
    case "projects":
      return <Projects key={projectsEpoch} />;
    case "inspect":
      return <Inspect />;
    case "connections":
      return <Connections />;
    default:
      return <SettingsScreen />;
  }
}

interface ModeResult {
  mode: WindowMode;
  error: string | null;
}

export function App() {
  useAppEvents();
  const [projectsEpoch, setProjectsEpoch] = useState(0);
  const [mini, setMini] = useState(false);
  const [switching, setSwitching] = useState(false);
  const currentMode = useRef<WindowMode>("full");
  const transitions = useRef<Promise<unknown>>(Promise.resolve());
  const queued = useRef(0);
  const [modeError, setModeError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [inputMode, setInputMode] = useState<InputMode>("typing");
  const appendTranscript = useCallback((text: string) => {
    setDraft((value) => value.trim() === "" ? text : `${value.trimEnd()} ${text}`);
  }, []);
  const dictation = useDictation(appendTranscript);

  const changeMode = useCallback((request: WindowModeRequest): Promise<ModeResult> => {
    queued.current++;
    setSwitching(true);
    const operation = transitions.current.then(async (): Promise<ModeResult> => {
      if (request === "status") return { mode: currentMode.current, error: null };
      const next = request === "toggle" ? (currentMode.current === "mini" ? "full" : "mini") : request;
      setModeError(null);
      try {
        // Release the full layout's minimum content size before GTK shrinks
        // the webview. Keep mini rendered until expansion finishes on return.
        if (next === "mini") flushSync(() => setMini(true));
        await setMiniMode(next === "mini");
        currentMode.current = next;
        setMini(next === "mini");
        return { mode: next, error: null };
      } catch (error) {
        setMini(currentMode.current === "mini");
        const message = errorOf(error).message;
        setModeError(message);
        return { mode: currentMode.current, error: message };
      }
    }).finally(() => {
      queued.current--;
      if (queued.current === 0) setSwitching(false);
    });
    transitions.current = operation;
    return operation;
  }, []);

  useEffect(() => {
    let active = true;
    const listener = onWindowModeRequest(([requestId, desired]) => {
      if (!active) return;
      void changeMode(desired)
        .then((result) => api.completeWindowMode(requestId, result.mode, result.error))
        .catch((error) => setModeError(errorOf(error).message));
    });
    void listener.catch((error) => {
      if (active) setModeError(errorOf(error).message);
    });
    return () => {
      active = false;
      void listener.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [changeMode]);

  const openFull = (target?: Screen) => {
    if (target !== undefined) useStore.getState().setScreen(target);
    void changeMode("full");
  };

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!event.repeat && (event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === "m") {
        event.preventDefault();
        void changeMode("toggle");
      } else if (event.key === "Escape") {
        if (dictation.status !== "idle") {
          void dictation.cancel();
        } else {
          const state = useStore.getState();
          const run = currentChatRun(state.runs, state.conversation.activeId);
          if (run?.status === "running") void state.stop(run.run);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [changeMode, dictation.status, dictation.cancel]);

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
  const composer = (
    <AgentComposer draft={draft} setDraft={setDraft} inputMode={inputMode}
      setInputMode={setInputMode} dictation={dictation}
      onConnections={() => openFull("connections")} />
  );
  const notices = (
    <>
      {modeError !== null && (
        <div className={`${shell.banner} fail`} role="alert">
          <span>{modeError}</span>
          <button className="link" onClick={() => setModeError(null)}>Dismiss</button>
        </div>
      )}
      {missingKey !== null && (
        <div className={`${shell.banner} fail`} role="alert">
          <span>No {missingKey} key is stored, so nothing can run yet.</span>
          <button className="link" onClick={() => openFull("connections")}>Open Connections</button>
        </div>
      )}
      {banner !== null && (
        <div className={`${shell.banner} ${banner.tone}`} role="status">
          <span>{banner.text}</span>
          <button className="link" onClick={dismiss}>Dismiss</button>
        </div>
      )}
    </>
  );

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
    <>
    {mini && (
      <MiniMode composer={composer} expand={() => openFull("chat")}
        switching={switching} notice={notices}
        onError={(error) => setModeError(errorOf(error).message)} />
    )}
    <div className={shell.shell} hidden={mini}>
      <nav className={shell.rail} aria-label="Screens">
        <div className={shell.wordmark}>Starkbot Neo</div>
        {TABS.map((tab) => (
          <button
            key={tab.id}
            className={shell.tab}
            aria-current={screen === tab.id ? "page" : undefined}
            onClick={() => {
              if (tab.id !== "chat") void dictation.cancel();
              setScreen(tab.id);
              if (tab.id === "projects") {
                setProjectsEpoch((value) => value + 1);
              }
            }}
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
        <button className={shell.miniToggle} disabled={switching || !ready}
          onClick={() => void changeMode("mini")} title="Mini mode (Ctrl+Shift+M)">
          Mini mode <span aria-hidden="true">↙</span>
        </button>
          <span>bridge v{catalog.bridgeVersion}</span>
          <span>{catalog.storePath}</span>
          {gaps > 0 && <span>{gaps} event gaps repaired</span>}
        </div>
      </nav>

      <main className={shell.main}>
        {notices}
        <div className={shell.screen}>
          {ready ? (
            <Screens screen={screen} projectsEpoch={projectsEpoch} composer={mini ? null : composer} />
          ) : (
            <p className={shell.loading}>opening the store…</p>
          )}
        </div>
      </main>
    </div>
    </>
  );
}
