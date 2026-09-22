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
import { Eval } from "./screens/Eval";
import { Inspect } from "./screens/Inspect";
import { Projects } from "./screens/Projects";
import { Runs } from "./screens/Runs";
import { SettingsScreen } from "./screens/SettingsScreen";
import { missingRequiredKey } from "./store/health";
import { useStore, type Screen } from "./store/store";
import shell from "./styles/shell.module.css";

const TABS: { id: Screen; label: string; icon: string }[] = [
  { id: "chat", label: "Chat", icon: "M4 4h16v12H9l-5 4V4Z" },
  { id: "runs", label: "Runs", icon: "M3 12h4l3-8 4 16 3-8h4" },
  { id: "eval", label: "Eval", icon: "M5 3h14v4l-5 5 5 5v4H5v-4l5-5-5-5V3Z" },
  { id: "projects", label: "Projects", icon: "M3 6h7l2 2h9v12H3V6Z" },
  { id: "inspect", label: "Inspect", icon: "M10 17a7 7 0 1 0 0-14 7 7 0 0 0 0 14Zm5-2 6 6" },
  // Its own rail entry rather than the thirteenth tab inside Settings: this
  // is where a key is typed and a subscription signed in, and a fresh
  // install cannot do anything at all until someone finds it.
  { id: "connections", label: "Connections", icon: "m9 15 6-6M8 10l-3 3a4 4 0 0 0 6 6l3-3m-4-8 3-3a4 4 0 0 1 6 6l-3 3" },
  { id: "settings", label: "Settings", icon: "M4 7h16M4 17h16M8 4v6m8 4v6" },
];

function Screens({ screen, projectsEpoch, chatEpoch, evalEpoch, composer }: {
  screen: Screen;
  projectsEpoch: number;
  chatEpoch: number;
  evalEpoch: number;
  composer: ReactNode;
}) {
  switch (screen) {
    case "chat":
      return <Chat key={chatEpoch} composer={composer} />;
    case "runs":
      return <Runs />;
    case "eval":
      return <Eval key={evalEpoch} />;
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
  const [chatEpoch, setChatEpoch] = useState(0);
  const [evalEpoch, setEvalEpoch] = useState(0);
  const [collapsed, setCollapsed] = useState(false);
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
    if (target === "chat") setChatEpoch((value) => value + 1);
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
    <div className={`${shell.shell} ${collapsed ? shell.collapsed : ""}`} hidden={mini}>
      <nav className={shell.rail} aria-label="Screens">
        <div className={shell.railHeader}>
          <div className={shell.wordmark}>Starkbot Neo</div>
          <button className={shell.collapseToggle} aria-expanded={!collapsed}
            aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
            title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
            onClick={() => setCollapsed((value) => !value)}>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M3 4h18v16H3V4Zm5 0v16" />
              <path d={collapsed ? "m12 9 3 3-3 3" : "m16 9-3 3 3 3"} />
            </svg>
          </button>
        </div>
        {TABS.map((tab) => (
          <button
            key={tab.id}
            className={shell.tab}
            aria-current={screen === tab.id ? "page" : undefined}
            aria-label={tab.label}
            title={collapsed ? tab.label : undefined}
            onClick={() => {
              if (tab.id !== "chat") void dictation.cancel();
              setScreen(tab.id);
              if (tab.id === "projects") {
                setProjectsEpoch((value) => value + 1);
              }
              if (tab.id === "eval") {
                setEvalEpoch((value) => value + 1);
              }
            }}
          >
            <svg className={shell.navIcon} viewBox="0 0 24 24" aria-hidden="true"><path d={tab.icon} /></svg>
            <span className={shell.navLabel}>{tab.label}</span>
            {/* The count is a second reading of the same state the Runs
                screen shows; it is never the only one. */}
            {tab.id === "runs" && running > 0 && (
              <span className={shell.count} aria-label={`${running} running`}>
                {collapsed ? running : `${running} running`}
              </span>
            )}
          </button>
        ))}
        <div className={shell.railFoot}>
        <button className={shell.miniToggle} disabled={switching || !ready}
          aria-label="Mini mode" onClick={() => void changeMode("mini")} title="Mini mode (Ctrl+Shift+M)">
          <span className={shell.navLabel}>Mini mode</span><span aria-hidden="true">↙</span>
        </button>
          <span title={`bridge v${catalog.bridgeVersion}`}>{collapsed ? "v" : "bridge v"}{catalog.bridgeVersion}</span>
          <span className={shell.navLabel}>{catalog.storePath}</span>
          {gaps > 0 && <span className={shell.navLabel}>{gaps} event gaps repaired</span>}
        </div>
      </nav>

      <main className={shell.main}>
        {notices}
        <div className={shell.screen}>
          {ready ? (
            <Screens screen={screen} projectsEpoch={projectsEpoch} chatEpoch={chatEpoch}
              evalEpoch={evalEpoch} composer={mini ? null : composer} />
          ) : (
            <p className={shell.loading}>opening the store…</p>
          )}
        </div>
      </main>
    </div>
    </>
  );
}
