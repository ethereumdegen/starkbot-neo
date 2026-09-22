import { useId, useRef, type Dispatch, type SetStateAction } from "react";

import type { Dictation } from "../hooks/useDictation";
import { currentChatRun } from "../store/runs";
import { useStore } from "../store/store";
import styles from "../styles/mini.module.css";

export type InputMode = "typing" | "microphone";

export interface ComposerProps {
  draft: string;
  setDraft: Dispatch<SetStateAction<string>>;
  inputMode: InputMode;
  setInputMode: (mode: InputMode) => void;
  dictation: Dictation;
  onConnections: () => void;
}

export function AgentComposer({
  draft, setDraft, inputMode, setInputMode, dictation, onConnections,
}: ComposerProps) {
  const id = useId();
  const editor = useRef<HTMLTextAreaElement>(null);
  const busy = useStore((state) => state.ui.busy);
  const ready = useStore((state) => state.ui.ready && state.ui.blocked === null);
  const live = useStore((state) =>
    currentChatRun(state.runs, state.conversation.activeId)?.status === "running",
  );
  const send = useStore((state) => state.send);
  const recording = dictation.status === "listening";
  const voiceBusy = dictation.status !== "idle";
  const canAccept = ready && !busy && !voiceBusy && draft.trim() !== "";

  const choose = (mode: InputMode) => {
    setInputMode(mode);
    if (mode === "typing") {
      if (recording) void dictation.stop();
      editor.current?.focus();
    } else if (dictation.status === "idle") {
      void dictation.start();
    }
  };

  const accept = async () => {
    if (!canAccept || useStore.getState().ui.busy) return;
    const submitted = draft;
    if (await send(submitted.trim())) {
      setDraft((value) => value === submitted ? "" : value);
    }
  };

  return (
    <form className={styles.composer} onSubmit={(event) => {
      event.preventDefault();
      void accept();
    }}>
      <div className={styles.toolbar}>
        <div className={styles.tabs} role="tablist" aria-label="Input method">
          {(["typing", "microphone"] as const).map((mode) => (
            <button
              key={mode}
              type="button"
              role="tab"
              id={`${id}-${mode}`}
              aria-selected={inputMode === mode}
              aria-controls={`${id}-input`}
              tabIndex={inputMode === mode ? 0 : -1}
              disabled={!ready || dictation.status === "starting"}
              onClick={() => choose(mode)}
              onKeyDown={(event) => {
                if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                  event.preventDefault();
                  const next = mode === "typing" ? "microphone" : "typing";
                  choose(next);
                  document.getElementById(`${id}-${next}`)?.focus();
                }
              }}
            >
              <svg viewBox="0 0 20 20" aria-hidden="true">
                {mode === "typing" ? (
                  <><rect x="2" y="4" width="16" height="11" rx="2" /><path d="M5 8h1m3 0h1m3 0h1M6 12h8" /></>
                ) : (
                  <><rect x="7" y="2" width="6" height="10" rx="3" /><path d="M4 9a6 6 0 0 0 12 0M10 15v3m-3 0h6" /></>
                )}
              </svg>
              {mode === "typing" ? "Typing" : "Microphone"}
            </button>
          ))}
        </div>
        <div className={`${styles.listening} ${recording ? styles.isListening : ""}`}>
          <div className={styles.spectrum} role="img" aria-label="Live microphone frequency spectrum">
            {dictation.spectrum.map((value, index) => (
              <span key={index} style={{ height: `${Math.max(2, value * 18)}px` }} />
            ))}
          </div>
          <span role="status">
            {recording ? "Listening" : dictation.status === "starting" ? "Starting…" :
              dictation.status === "transcribing" ? "Transcribing…" : "Mic off"}
          </span>
        </div>
        {inputMode === "microphone" && (
          <button
            type="button"
            className={styles.micControl}
            disabled={!ready || (voiceBusy && !recording)}
            onClick={() => void (recording ? dictation.stop() : dictation.start())}
          >
            {recording ? "Stop mic" : "Start mic"}
          </button>
        )}
      </div>
      <div className={styles.inputRow} id={`${id}-input`} role="tabpanel" aria-labelledby={`${id}-${inputMode}`}>
        <textarea
          ref={editor}
          rows={2}
          value={draft}
          aria-label="Message"
          placeholder={recording ? "Listening… stop the mic to review your words" :
            inputMode === "microphone" ? "Your transcript appears here. Edit it, then Accept." :
              live ? "Give the agent another instruction…" : "Ask Starkbot to do something…"}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              accept();
            }
          }}
        />
        <button type="submit" className={styles.accept} disabled={!canAccept}
          title={live ? "Accept and steer the current agent turn" : "Accept and send to the agent"}>
          Accept <span aria-hidden="true">↵</span>
        </button>
      </div>
      <div className={styles.inputFoot}>
        <span>{inputMode === "microphone" ? "OpenAI speech-to-text · review before sending" : "Enter to accept · Shift+Enter for a new line"}</span>
        {voiceBusy && <button type="button" className="link" onClick={() => void dictation.cancel()}>Cancel mic</button>}
        {live && <span className={styles.steering}>Steers current turn</span>}
      </div>
      {dictation.error !== null && (
        <div className={styles.error} role="alert">
          <span>{dictation.error}</span>
          <button type="button" className="link" onClick={onConnections}>Connections</button>
        </div>
      )}
    </form>
  );
}
