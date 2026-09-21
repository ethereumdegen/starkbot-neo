import { useEffect, useMemo, useRef, useState } from "react";

import type { MessageView } from "../bridge/api";
import { AskCard, ConfirmCard } from "../components/GateCard";
import { RunSteps } from "../components/RunTrace";
import { frontGate } from "../store/gates";
import { useStore } from "../store/store";
import { currentChatRun, elapsedMs, type RunRecord } from "../store/runs";
import { modelId } from "../store/settings";
import chat from "../styles/chat.module.css";
import panes from "../styles/panes.module.css";
import trace from "../styles/trace.module.css";

function when(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/**
 * The run a stored row came out of.
 *
 * The core stamps every row it writes during a turn with `meta.run`, and
 * that is the only join there is: a `MessageView` has no run column, and
 * matching rows to turns by timestamp would file a row under whichever turn
 * happened to be closest.
 */
function runOf(message: MessageView): string | null {
  const meta = message.meta;
  if (typeof meta !== "object" || meta === null || Array.isArray(meta)) {
    return null;
  }
  return typeof meta.run === "string" ? meta.run : null;
}

/**
 * The failure codes that mean "a key this turn needed is not stored".
 *
 * This used to be a regex over the failure text, and it existed only to
 * rewrite a sentence the backend had already written. `TurnFailed` now
 * carries the producer's own classification, so the sentence is shown as
 * sent and the code decides whether Connections is worth offering.
 *
 * Two spellings, because two layers publish the event: `neo-agent`'s
 * `AgentError::NoKey` and the desktop's `RuntimeError::MissingKey`.
 */
const MISSING_KEY_CODES: Record<string, true> = { agent_no_key: true, missing_key: true };

function Bubble({ message, steered }: { message: MessageView; steered: boolean }) {
  const tone = message.role === "user" ? chat.user : chat.agent;
  // `kind` exists so a front end need not re-derive intent from the text: a
  // tool result is a card, not a paragraph of the conversation.
  const card = message.kind === "result" || message.role === "tool";
  return (
    <li className={`${chat.bubble} ${tone} ${card ? chat.card : ""}`}>
      <div className={chat.bubbleHead}>
        <span>{message.role}</span>
        {message.kind !== "text" && <span className={chat.kind}>{message.kind}</span>}
        {steered && <span className={chat.steered}>steered</span>}
        <span className={chat.time}>{when(message.at)}</span>
      </div>
      <div className={chat.text}>{message.text}</div>
    </li>
  );
}

export function Chat() {
  const conversations = useStore((state) => state.conversation.conversations);
  const activeId = useStore((state) => state.conversation.activeId);
  const messages = useStore((state) => state.conversation.messages);
  const runs = useStore((state) => state.runs);
  const busy = useStore((state) => state.ui.busy);
  const send = useStore((state) => state.send);
  const stop = useStore((state) => state.stop);
  const select = useStore((state) => state.selectConversation);
  const create = useStore((state) => state.newConversation);
  const rename = useStore((state) => state.renameConversation);
  const model = useStore((state) => modelId(state.settings));
  const setScreen = useStore((state) => state.setScreen);
  const gates = useStore((state) => state.gates);
  const resolveConfirm = useStore((state) => state.resolveConfirm);
  const answerAsk = useStore((state) => state.answerAsk);

  const [draft, setDraft] = useState("");
  const [renaming, setRenaming] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [showTurn, setShowTurn] = useState(false);
  const tail = useRef<HTMLDivElement | null>(null);

  const current: RunRecord | null = useMemo(
    () => currentChatRun(runs, activeId),
    [runs, activeId],
  );

  // One card at a time, and the oldest first: two questions side by side
  // ask the user to answer for two runs at once, and the one they read is
  // whichever happened to render on top.
  const gate = useMemo(() => frontGate(gates), [gates]);

  const live = current !== null && current.status === "running";
  const liveRun = live && current !== null ? current.run : null;
  const failedForKey = current?.code != null && MISSING_KEY_CODES[current.code] === true;
  // What the closed pane is hiding, so the button is worth pressing: a live
  // turn says how much work is behind it, a settled one just offers itself.
  const turnLabel =
    current === null
      ? "Turn"
      : live
        ? `Turn · ${current.steps.length} step${current.steps.length === 1 ? "" : "s"}`
        : "Turn";

  /**
   * The rows the live turn is writing are painted by the turn itself.
   *
   * The core writes each observation to the thread as the step ends, so a
   * thread that rendered both would show every tool card twice while the
   * turn ran. The user's own steers are not hidden — they are what the turn
   * is being told, and seeing them arrive is the point.
   */
  const rows = useMemo(
    () =>
      liveRun === null
        ? messages
        : messages.filter((row) => row.role === "user" || runOf(row) !== liveRun),
    [messages, liveRun],
  );

  useEffect(() => {
    if (!live) {
      return;
    }
    // A running turn shows its own elapsed time; nothing else here ticks.
    const timer = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(timer);
  }, [live]);

  /**
   * Escape stops the turn, wherever the focus is.
   *
   * The Stop button is across the window from the composer, which is where
   * the hands are when a turn goes wrong; a key that only worked while the
   * button had focus would be a key nobody could reach in time.
   */
  useEffect(() => {
    if (liveRun === null) {
      return;
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        void stop(liveRun);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [liveRun, stop]);

  // A card raised below a long thread is a card nobody sees, and the run
  // waits for it — so it scrolls itself into view like the rest of the tail.
  const gateId = gate === null ? null : gate.kind === "confirm" ? gate.confirm.id : gate.ask.id;
  useEffect(() => {
    tail.current?.scrollIntoView({ block: "end" });
  }, [messages.length, current?.steps.length, current?.stream, gateId]);

  const active = conversations.find((row) => row.id === activeId) ?? null;

  const submit = () => {
    const text = draft.trim();
    if (text === "" || busy) {
      return;
    }
    setDraft("");
    void send(text);
  };

  return (
    <div className={`${panes.columns} ${showTurn ? panes.chat : panes.chatWide}`}>
      <section className={panes.pane} aria-label="Conversations">
        <div className={panes.head}>
          <h2>Threads</h2>
          <button className={panes.spacer} onClick={() => void create()} disabled={busy}>
            New
          </button>
        </div>
        <div className={`${panes.body} ${panes.tight}`}>
          {conversations.length === 0 && (
            <p className={panes.empty}>No thread yet. Say something and one starts.</p>
          )}
          {conversations.map((row) => (
            <button
              key={row.id}
              className={panes.row}
              aria-selected={row.id === activeId}
              onClick={() => void select(row.id)}
            >
              <span className={panes.rowTitle}>{row.title ?? "Untitled"}</span>
              <span className={panes.rowMeta}>{when(row.updated_at)}</span>
            </button>
          ))}
        </div>
      </section>

      <section className={panes.pane} aria-label="Conversation">
        <div className={panes.head}>
          {renaming === null ? (
            <>
              <h2>{active?.title ?? "No thread"}</h2>
              {active !== null && (
                <button
                  className={panes.spacer}
                  onClick={() => setRenaming(active.title ?? "")}
                >
                  Rename
                </button>
              )}
              {/* The turn pane is a detail view, and the thread is the
                  screen: it starts closed so the conversation gets the
                  width, and the live step count says what is behind it. */}
              <button
                className={active === null ? panes.spacer : undefined}
                aria-expanded={showTurn}
                onClick={() => setShowTurn(!showTurn)}
              >
                {showTurn ? "Hide turn" : turnLabel}
              </button>
            </>
          ) : (
            <form
              className={chat.rename}
              onSubmit={(event) => {
                event.preventDefault();
                if (activeId !== null && renaming.trim() !== "") {
                  void rename(activeId, renaming.trim());
                }
                setRenaming(null);
              }}
            >
              <input
                autoFocus
                value={renaming}
                aria-label="Conversation title"
                onChange={(event) => setRenaming(event.target.value)}
              />
              <button type="submit" className="primary">
                Save
              </button>
              <button type="button" onClick={() => setRenaming(null)}>
                Cancel
              </button>
            </form>
          )}
        </div>
        <div className={panes.body}>
          {rows.length === 0 && !live && <p className={panes.empty}>Nothing said yet.</p>}
          <ul className={chat.thread}>
            {rows.map((message) => (
              <Bubble
                key={message.id}
                message={message}
                // A user row the core stamped with a run is one that was
                // said into a turn that was already going.
                steered={message.role === "user" && runOf(message) !== null}
              />
            ))}
            {live && current !== null && (
              <li className={chat.turn} aria-label="Turn in progress">
                <RunSteps record={current} />
                {current.stream !== "" && (
                  <div className={`${chat.bubble} ${chat.agent} ${chat.streaming}`}>
                    <div className={chat.bubbleHead}>
                      <span>assistant</span>
                      <span className={chat.kind}>writing</span>
                    </div>
                    <div className={chat.text}>{current.stream}</div>
                  </div>
                )}
              </li>
            )}
            {/* A turn that ended badly belongs in the thread, not only in a
                pane beside it: the answer is missing, and the reason is what
                the reader is looking for. When the reason is a key nobody
                has stored, the row carries the way to fix it. */}
            {current !== null && current.error !== null && (
              <li className={`${chat.bubble} ${chat.agent} ${chat.failure}`}>
                <div className={chat.bubbleHead}>
                  <span>turn</span>
                  <span className={chat.kind}>{current.status}</span>
                </div>
                {/* The backend's own sentence, not a second one written
                    here: `AgentError::NoKey` and `RuntimeError::MissingKey`
                    both name the account and the command that stores it. */}
                <div className={chat.text}>{current.error}</div>
                {failedForKey && (
                  <button className="link" onClick={() => setScreen("connections")}>
                    Open Connections
                  </button>
                )}
              </li>
            )}
            {/* Last in the thread and never scrolled past: a card is a run
                standing still, so it sits below everything the turn has
                said, where the eye already is and where the next thing to
                do belongs. */}
            {gate?.kind === "confirm" && (
              <ConfirmCard
                key={gate.confirm.id}
                confirm={gate.confirm}
                pending={gates.pending[gate.confirm.id] === true}
                onResolve={(outcome) => void resolveConfirm(gate.confirm.id, outcome)}
              />
            )}
            {gate?.kind === "ask" && (
              <AskCard
                key={gate.ask.id}
                ask={gate.ask}
                pending={gates.pending[gate.ask.id] === true}
                onAnswer={(answer) => void answerAsk(gate.ask.id, answer)}
              />
            )}
          </ul>
          <div ref={tail} />
        </div>
        {live && current !== null && (
          <div className={chat.status} aria-live="polite">
            {/* `TurnCost` republishes the running total after every model
                round trip, so these numbers move while the turn does. The
                dashes are the window before the first round trip returns —
                a zero there would read as "this turn is costing nothing". */}
            {model ?? "no model"} · {Math.round(elapsedMs(current, now) / 1000)}s ·{" "}
            {current.steps.length} step{current.steps.length === 1 ? "" : "s"} ·{" "}
            {current.usage?.input_tokens ?? "—"} in / {current.usage?.output_tokens ?? "—"} out
          </div>
        )}
        <form
          className={chat.composer}
          onSubmit={(event) => {
            event.preventDefault();
            submit();
          }}
        >
          <textarea
            rows={2}
            value={draft}
            placeholder={live ? "Say something into this turn" : "Ask for something"}
            aria-label="Message"
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                submit();
              }
            }}
          />
          {/* A turn is already running: what is typed steers it rather than
              starting a second one, and the label says so before it is
              pressed. */}
          <button type="submit" className="primary" disabled={busy}>
            {live ? "Steer" : "Send"}
          </button>
        </form>
      </section>

      {showTurn && (
      <section className={panes.pane} aria-label="Current turn">
        <div className={panes.head}>
          <h2>Turn</h2>
          {live && (
            <>
              <span className={`${panes.spacer} ${trace.usage}`}>
                {Math.round(elapsedMs(current, now) / 1000)}s · {current.steps.length} step
                {current.steps.length === 1 ? "" : "s"}
              </span>
              {/* Cancelling releases the run's token. A Chrome window it
                  already opened stays open — the banner says so rather than
                  implying the machine went back to how it was. */}
              <button className="danger" onClick={() => void stop(current.run)}>
                Stop
              </button>
            </>
          )}
          {/* Closing lives here as well as on the thread's header: with this
              pane open the thread is narrow, and its own toggle is the first
              thing the cramped header drops. A pane you cannot shut is a
              pane that eats the window. */}
          <button
            className={live ? undefined : panes.spacer}
            aria-label="Hide the turn pane"
            onClick={() => setShowTurn(false)}
          >
            Close
          </button>
        </div>
        <div className={panes.body}>
          {current === null ? (
            <p className={panes.empty}>No turn in this thread yet.</p>
          ) : (
            <>
              <div className={trace.stepHead}>
                <span>{current.status}</span>
                {current.exhausted && <span className={trace.stale}>step budget spent</span>}
              </div>
              <RunSteps record={current} />
              {current.error !== null && <div className={trace.answer}>{current.error}</div>}
              {current.usage !== null && (
                <div className={trace.usage}>
                  {current.usage.input_tokens} in · {current.usage.output_tokens} out ·{" "}
                  {current.usage.cached_input_tokens} cached · {current.usage.requests} requests
                </div>
              )}
            </>
          )}
        </div>
      </section>
      )}
    </div>
  );
}
