import type { ReactNode } from "react";

import { startWindowDrag } from "../bridge/window";
import { frontGate } from "../store/gates";
import { currentChatRun } from "../store/runs";
import { useStore } from "../store/store";
import styles from "../styles/mini.module.css";

export function MiniMode({ composer, expand, switching, notice, onError }: {
  composer: ReactNode;
  expand: () => void;
  switching: boolean;
  notice: ReactNode;
  onError: (error: unknown) => void;
}) {
  const current = useStore((state) => currentChatRun(state.runs, state.conversation.activeId));
  const active = useStore((state) => state.conversation.conversations.find(
    (row) => row.id === state.conversation.activeId,
  ));
  const gate = useStore((state) => frontGate(state.gates));
  const stop = useStore((state) => state.stop);
  const busy = useStore((state) => state.ui.busy);
  const ready = useStore((state) => state.ui.ready);
  const latest = useStore((state) => {
    const messages = state.conversation.messages;
    for (let index = messages.length - 1; index >= 0; index--) {
      const message = messages[index];
      if (message.role === "assistant" && message.kind === "text") return message;
    }
    return null;
  });
  const live = current?.status === "running";
  const response = current?.error ?? current?.stream ?? latest?.text;
  const summary = response || current?.text || latest?.text;

  return (
    <main className={styles.mini} aria-label="Starkbot Neo mini mode">
      <header className={styles.titlebar}>
        <div className={styles.dragHandle}
          onPointerDown={(event) => {
            if (event.button === 0) void startWindowDrag().catch(onError);
          }}
          title="Drag to move the mini window"
        >
          <span className={styles.brand}>STARKBOT NEO</span>
          <span className={styles.modeLabel}>MINI</span>
          <span className={styles.grip} aria-hidden="true">⠿</span>
        </div>
        <button type="button" className={styles.expand} onClick={expand} disabled={switching}
          aria-label="Switch to full mode" title="Full mode (Ctrl+Shift+M)">
          <svg viewBox="0 0 20 20" aria-hidden="true"><path d="M12 3h5v5M17 3l-6 6M8 17H3v-5m0 5 6-6" /></svg>
          Full mode
        </button>
      </header>
      <div className={styles.miniBody}>
        {composer}
        {notice}
      </div>
      <footer className={styles.runStatus}>
        <span className={`${styles.statusDot} ${live ? styles.working : ""}`} />
        <span className={styles.threadName} title={active?.title ?? "New conversation"}>
          {!ready ? "Opening the store…" : live ? "Agent working" : active?.title ?? "New conversation"}
        </span>
        {gate !== null ? (
          <button type="button" className={styles.review} onClick={expand} disabled={switching}>
            {gate.kind === "confirm" ? "Approval needed" : "Agent has a question"} · Review
          </button>
        ) : summary ? (
          <button type="button" className={styles.result} onClick={expand} disabled={switching} title={summary}>
            {summary}
          </button>
        ) : <span className={styles.hint}>Your agent, without the full window</span>}
        {live && current !== null && (
          <button type="button" className={styles.stop} disabled={busy} onClick={() => void stop(current.run)}>
            Stop agent
          </button>
        )}
      </footer>
    </main>
  );
}
