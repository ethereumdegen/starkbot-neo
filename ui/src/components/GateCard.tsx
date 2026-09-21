import { useState } from "react";

import type { AskView, ConfirmView, Usage } from "../bridge/api";
import chat from "../styles/chat.module.css";

/**
 * What the run thinks this will cost, when anybody can say.
 *
 * `unpriced` is plan-backed work: there is no dollar figure to show and
 * inventing one would be a number nobody can reconcile with a bill, so the
 * line is omitted rather than filled with a zero.
 */
function cost(usage: Usage): string | null {
  const { usd } = usage;
  if (usd.kind === "unpriced") {
    return null;
  }
  const amount = `$${usd.usd.toFixed(2)}`;
  return usd.kind === "estimated" ? `about ${amount}` : amount;
}

/**
 * A run standing still until somebody says yes.
 *
 * The sentence is the backend's, verbatim: it is written in the product's
 * voice and says what is about to happen in words the user can check against
 * the screen behind the window. This card recomposes nothing from `cause` —
 * that is the machine-ish tag, shown as the reason beside the sentence, not
 * as a substitute for it.
 *
 * Both buttons go inert the moment either is pressed. The card stays until
 * the run publishes its resolve, because that is the only moment the answer
 * has actually landed — a card that vanished on click would have this window
 * claiming an approval the run may never have received.
 */
export function ConfirmCard({
  confirm,
  pending,
  onResolve,
}: {
  confirm: ConfirmView;
  pending: boolean;
  onResolve: (outcome: "confirmed" | "denied") => void;
}) {
  const priced = confirm.estimated_cost === null ? null : cost(confirm.estimated_cost);
  return (
    <li className={`${chat.bubble} ${chat.gate}`} aria-label="Confirm">
      <div className={chat.bubbleHead}>
        <span>confirm</span>
        <span className={chat.gateCause}>{confirm.cause}</span>
        <span className={chat.time}>
          until{" "}
          {new Date(confirm.expires_at).toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
          })}
        </span>
      </div>
      <div className={chat.text}>{confirm.action_sentence}</div>
      {confirm.context !== null && <div className={chat.gateContext}>{confirm.context}</div>}
      {priced !== null && <div className={chat.gateContext}>Costs {priced}.</div>}
      <div className={chat.gateActions}>
        <button
          type="button"
          className="primary"
          disabled={pending}
          onClick={() => onResolve("confirmed")}
        >
          Approve
        </button>
        <button
          type="button"
          className="danger"
          disabled={pending}
          onClick={() => onResolve("denied")}
        >
          Deny
        </button>
        {pending && <span className={chat.gateSent}>sent</span>}
      </div>
    </li>
  );
}

/**
 * A run that cannot go on until it is told something.
 *
 * Two shapes, and the wire says which: a non-empty `options` is answered
 * with one of its strings — that is how a sign-in hand-over arrives, as a
 * single "I'm ready" — and an empty one is free text, because the run hit a
 * field only the user knows the value of.
 */
export function AskCard({
  ask,
  pending,
  onAnswer,
}: {
  ask: AskView;
  pending: boolean;
  onAnswer: (answer: string) => void;
}) {
  const [draft, setDraft] = useState("");
  const typed = draft.trim();

  return (
    <li className={`${chat.bubble} ${chat.gate}`} aria-label="Question">
      <div className={chat.bubbleHead}>
        <span>question</span>
      </div>
      <div className={chat.text}>{ask.question}</div>
      {ask.options.length > 0 ? (
        <div className={chat.gateActions}>
          {ask.options.map((option) => (
            <button
              key={option}
              type="button"
              className="primary"
              disabled={pending}
              onClick={() => onAnswer(option)}
            >
              {option}
            </button>
          ))}
          {pending && <span className={chat.gateSent}>sent</span>}
        </div>
      ) : (
        <form
          className={chat.gateActions}
          onSubmit={(event) => {
            event.preventDefault();
            if (typed !== "" && !pending) {
              onAnswer(typed);
            }
          }}
        >
          <input
            autoFocus
            value={draft}
            aria-label="Answer"
            disabled={pending}
            onChange={(event) => setDraft(event.target.value)}
          />
          <button type="submit" className="primary" disabled={pending || typed === ""}>
            Answer
          </button>
          {pending && <span className={chat.gateSent}>sent</span>}
        </form>
      )}
    </li>
  );
}
