import corpusText from "../../../fixtures/cards/envelopes.jsonl?raw";

import { describe, expect, it } from "vitest";

import type { AppEvent, AskView, ConfirmView, Envelope } from "../bridge/api";
import { claimGate, frontGate, releaseGate } from "./gates";
import { applyEnvelope, initialState, type AppState } from "./reduce";
import { elapsedMs } from "./runs";
import { tally } from "./trace";

const RUN = "0192f2cd-0000-7000-8000-000000000001";
const OTHER_RUN = "0192f2cd-0000-7000-8000-000000000002";
const CONVERSATION = "0192f2cd-0000-7000-8000-00000000000a";

let clock = 0;

function envelope(event: AppEvent, seq: number, atMs = (clock += 1000)): Envelope {
  return { seq, at: new Date(atMs).toISOString(), event };
}

function play(events: [AppEvent, number][], from: AppState = initialState): AppState {
  return events.reduce((state, [event, seq]) => applyEnvelope(state, envelope(event, seq)), from);
}

/**
 * The card corpus both front ends are tested against.
 *
 * One `neo_core::Envelope` per line, verbatim wire JSON: the TUI's insta
 * goldens deserialize the same lines into `Envelope` and feed them to its
 * reducer, so the two front ends cannot quietly disagree about what a
 * confirm looks like — a field renamed in Rust breaks both at once.
 */
function corpus(): Envelope[] {
  return corpusText
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line) => JSON.parse(line) as Envelope);
}

describe("a turn assembling itself from its events", () => {
  it("pairs each step with its observation and keeps the order it ran in", () => {
    clock = 1_700_000_000_000;
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [
        {
          type: "turn_step",
          run: RUN,
          step: 0,
          thought: "find the price",
          action: {
            kind: "browse",
            target: "https://example.com/pricing",
            goal: "read the per-seat price",
            text: null,
          },
        },
        2,
      ],
      [{ type: "turn_note", run: RUN, step: 0, line: "launched chrome" }, 3],
      [
        { type: "turn_step_done", run: RUN, step: 0, observation: "$20 per seat", duration_ms: 4200 },
        4,
      ],
      [
        {
          type: "turn_step",
          run: RUN,
          step: 1,
          thought: "answer",
          action: { kind: "answer", target: null, goal: null, text: "$20 per seat" },
        },
        5,
      ],
      [
        {
          type: "turn_finished",
          run: RUN,
          text: "$20 per seat.",
          steps: 2,
          exhausted: false,
          usage: {
            input_tokens: 1200,
            output_tokens: 300,
            cached_input_tokens: 900,
            requests: 2,
          },
        },
        6,
      ],
    ]);

    const record = state.runs.byId[RUN];
    expect(state.runs.order).toEqual([RUN]);
    expect(record.kind).toBe("chat");
    expect(record.conversation).toBe(CONVERSATION);
    expect(record.status).toBe("finished");
    expect(record.text).toBe("$20 per seat.");
    expect(record.usage?.requests).toBe(2);
    expect(record.steps.map((step) => step.step)).toEqual([0, 1]);
    expect(record.steps[0]).toMatchObject({
      thought: "find the price",
      observation: "$20 per seat",
      durationMs: 4200,
      notes: ["launched chrome"],
    });
    expect(record.steps[0].action.target).toBe("https://example.com/pricing");
    // The second step is still in flight: an action chosen, nothing observed.
    expect(record.steps[1].observation).toBeNull();
    expect(elapsedMs(record, 0)).toBe(5000);
  });

  it("follows a run it never started, from the first event that names it", () => {
    clock = 1_700_000_000_000;
    const state = play([
      [
        {
          type: "turn_step_done",
          run: OTHER_RUN,
          step: 3,
          observation: "clicked Send",
          duration_ms: 90,
        },
        11,
      ],
    ]);

    const record = state.runs.byId[OTHER_RUN];
    expect(record.status).toBe("running");
    expect(record.steps).toHaveLength(1);
    expect(record.steps[0]).toMatchObject({ step: 3, observation: "clicked Send", thought: "" });
  });
});

describe("a turn the window is watching happen", () => {
  it("grows the answer out of its slices and refuses one it has already seen", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [{ type: "turn_delta", run: RUN, seq: 0, text: "The price " }, 2],
      [{ type: "turn_delta", run: RUN, seq: 1, text: "is $20" }, 3],
      // A reconnect replays the last slice; appending it again would
      // duplicate two words in the middle of the sentence.
      [{ type: "turn_delta", run: RUN, seq: 1, text: "is $20" }, 4],
      [
        {
          type: "turn_cost",
          run: RUN,
          usage: {
            input_tokens: 800,
            output_tokens: 40,
            cached_input_tokens: 0,
            requests: 1,
          },
        },
        5,
      ],
      [{ type: "turn_steered", run: RUN, text: "per seat, not per org" }, 6],
    ]);

    const record = state.runs.byId[RUN];
    expect(record.stream).toBe("The price is $20");
    expect(record.status).toBe("running");
    // The running total, so the status line can show what the turn is
    // costing before it ends.
    expect(record.usage?.output_tokens).toBe(40);
    expect(record.steers).toEqual(["per seat, not per org"]);
  });

  it("files a navigator line under the step that was open when it arrived", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [
        {
          type: "turn_step",
          run: RUN,
          step: 0,
          thought: "open the pricing page",
          action: { kind: "browse", target: "https://example.com", goal: "read it", text: null },
        },
        2,
      ],
      [{ type: "nav_step", run: RUN, step: 7, line: "launched chrome", kind: { kind: "launch" } }, 3],
      [
        { type: "turn_step_done", run: RUN, step: 0, observation: "$20 per seat", duration_ms: 900 },
        4,
      ],
      // Between two steps nothing is open: a line here belongs to no card
      // rather than to the one that just closed.
      [{ type: "nav_step", run: RUN, step: 8, line: "closed chrome", kind: { kind: "summary" } }, 5],
    ]);

    const record = state.runs.byId[RUN];
    expect(record.steps[0].nav.map((entry) => entry.line)).toEqual(["launched chrome"]);
    expect(record.kind).toBe("chat");
  });
});

describe("a run that ends badly", () => {
  it("moves running → failed and stops the clock", () => {
    clock = 1_700_000_000_000;
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [{ type: "turn_failed", run: RUN, error: "the model returned no action" }, 2],
    ]);

    const record = state.runs.byId[RUN];
    expect(record.status).toBe("failed");
    expect(record.error).toBe("the model returned no action");
    expect(record.endedAt).not.toBeNull();
    expect(elapsedMs(record, 9_999_999_999_999)).toBe(1000);
  });

  it("calls a cancelled run stopped, not broken", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [{ type: "turn_failed", run: RUN, error: "cancelled" }, 2],
    ]);

    expect(state.runs.byId[RUN].status).toBe("cancelled");
  });

  it("tells a turn the user stopped from one that answered", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      // A stopped turn still finishes, with the half answer it had: the
      // marker note is the only thing that says it was stopped.
      [{ type: "turn_note", run: RUN, step: 0, line: "stopped" }, 2],
      [
        {
          type: "turn_finished",
          run: RUN,
          text: "The price is",
          steps: 1,
          exhausted: false,
          usage: null,
        },
        3,
      ],
    ]);

    const record = state.runs.byId[RUN];
    expect(record.status).toBe("cancelled");
    expect(record.text).toBe("The price is");
    // The marker is the run's, not a step's: it must not draw a card for
    // work that never ran.
    expect(record.steps).toEqual([]);
  });
});

describe("the seq contract", () => {
  it("asks for a re-bootstrap when a seq is skipped", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 4],
      [{ type: "turn_note", run: RUN, step: 0, line: "first" }, 5],
      [{ type: "turn_note", run: RUN, step: 0, line: "third" }, 7],
    ]);

    expect(state.sync.needsBootstrap).toBe(true);
    expect(state.sync.gaps).toBe(1);
    expect(state.sync.lastSeq).toBe(7);
  });

  it("stays quiet while the stream is contiguous", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 4],
      [{ type: "turn_note", run: RUN, step: 0, line: "first" }, 5],
      [{ type: "turn_note", run: RUN, step: 0, line: "second" }, 6],
    ]);

    expect(state.sync.needsBootstrap).toBe(false);
    expect(state.sync.gaps).toBe(0);
  });

  it("takes the backend's lag notice as a gap, though its seq did not move", () => {
    const state = play([
      [{ type: "turn_note", run: RUN, step: 0, line: "first" }, 9],
      [{ type: "notice", level: "warning", code: "event_gap", text: "dropped 12 events" }, 9],
    ]);

    expect(state.sync.needsBootstrap).toBe(true);
    expect(state.sync.lastSeq).toBe(9);
  });
});

describe("the thread", () => {
  it("appends only messages of the conversation on screen, and never twice", () => {
    const seeded: AppState = {
      ...initialState,
      conversation: { ...initialState.conversation, activeId: CONVERSATION },
    };
    const message = {
      id: "0192f2cd-0000-7000-8000-0000000000ff",
      conversation_id: CONVERSATION,
      role: "user" as const,
      kind: "text" as const,
      text: "what does it cost",
      at: 1_700_000_000_000,
      meta: null,
    };
    const elsewhere = { ...message, id: "0192f2cd-0000-7000-8000-0000000000fe", conversation_id: "x" };

    const state = play(
      [
        [{ type: "message", message }, 1],
        [{ type: "message", message }, 2],
        [{ type: "message", message: elsewhere }, 3],
      ],
      seeded,
    );

    expect(state.conversation.messages).toHaveLength(1);
    // A thread that moved off screen still moved the switcher.
    expect(state.conversation.listStale).toBe(true);
  });

  it("marks the thread stale when the conversation it is showing is reset", () => {
    const seeded: AppState = {
      ...initialState,
      conversation: { ...initialState.conversation, activeId: CONVERSATION },
    };
    const state = play([[{ type: "conversation_reset", conversation_id: CONVERSATION }, 1]], seeded);

    expect(state.conversation.threadStale).toBe(true);
  });
});

describe("a suite", () => {
  it("replaces a case's row when it finishes instead of listing it twice", () => {
    const state = play([
      [{ type: "eval_case", run: RUN, index: 1, total: 2, case: "mail.send", state: { state: "started" } }, 1],
      [
        {
          type: "eval_case",
          run: RUN,
          index: 1,
          total: 2,
          case: "mail.send",
          state: { state: "passed", runs: 3 },
        },
        2,
      ],
      [
        {
          type: "eval_case",
          run: RUN,
          index: 2,
          total: 2,
          case: "notes.new",
          state: { state: "skipped", reason: "Notes is not installed" },
        },
        3,
      ],
    ]);

    const rows = state.trace.evals[RUN];
    expect(rows).toHaveLength(2);
    expect(tally(rows)).toEqual({ passed: 1, failed: 0, skipped: 1, running: 0, total: 2 });
    // The suite is one run, and the Runs list has to be able to show it.
    expect(state.runs.byId[RUN].kind).toBe("eval");
  });
});

describe("navigator lines", () => {
  it("keeps every line in order and the decision's structure with it", () => {
    const state = play([
      [{ type: "nav_step", run: RUN, step: 0, line: "launched chrome", kind: { kind: "launch" } }, 1],
      [
        {
          type: "nav_step",
          run: RUN,
          step: 1,
          line: "  412 ms  click       Sign in",
          kind: {
            kind: "decision",
            surface: "browser",
            operation: "click",
            label: "Sign in",
            operation_confidence: 0.94,
            target_confidence: 0.81,
            candidates: 12,
            stale: false,
            typed_chars: null,
            observe_ms: 40,
            jev_ms: 300,
            text_ms: 20,
            act_ms: 52,
            elapsed_ms: 412,
            safety: [["outward", 0.02]],
          },
        },
        2,
      ],
    ]);

    const entries = state.trace.nav[RUN];
    expect(entries.map((entry) => entry.kind.kind)).toEqual(["launch", "decision"]);
    const decision = entries[1].kind;
    expect(decision.kind === "decision" && decision.operation).toBe("click");
    expect(state.runs.byId[RUN].kind).toBe("nav");
  });
});

describe("cards, against the corpus both front ends read", () => {
  const lines = corpus();
  const [confirmAsked, confirmAnswered, readyAsked, readyAnswered, fieldAsked, fieldAnswered] =
    lines;

  function replay(...envelopes: Envelope[]): AppState {
    return envelopes.reduce((state, envelope) => applyEnvelope(state, envelope), initialState);
  }

  it("raises the confirm the wire describes, with the sentence and the page it is about", () => {
    const state = replay(confirmAsked);

    expect(state.gates.confirms).toHaveLength(1);
    const card = state.gates.confirms[0];
    // The sentence is the backend's and is shown verbatim; `cause` is the
    // tag it was raised under, not a second version of the sentence.
    expect(card.action_sentence).toMatch(/pay/i);
    expect(card.cause).toBe("safety:spends");
    expect(card.context).toContain("https://");
    // Q2 has no remembered allows, and a card offering one would promise a
    // decision nothing honours.
    expect(card.can_remember).toBe(false);
    expect(frontGate(state.gates)).toEqual({ kind: "confirm", confirm: card });
  });

  it("takes the card down when the run resolves it, whoever answered", () => {
    const state = replay(confirmAsked, confirmAnswered);

    expect(state.gates.confirms).toEqual([]);
    expect(frontGate(state.gates)).toBeNull();
  });

  it("raises no ghost when the resolve overtakes its own request", () => {
    // The run publishes the resolve while the request is still crossing the
    // IPC boundary. A window that ignored the early resolve would show a
    // card nothing is waiting on, and nothing would ever take it down.
    const state = replay(confirmAnswered, confirmAsked);

    expect(state.gates.confirms).toEqual([]);
    // And the memory of it is spent, not kept: that id can never be asked
    // again, so holding it would only be a leak.
    expect(state.gates.settled).toEqual([]);
  });

  it("does not raise a second card when a request is republished", () => {
    const state = replay(confirmAsked, confirmAsked);

    expect(state.gates.confirms).toHaveLength(1);
  });

  it("offers the hand-over's one option and clears it on the answer", () => {
    const asked = replay(readyAsked);

    expect(asked.gates.asks).toHaveLength(1);
    const ask: AskView = asked.gates.asks[0];
    expect(ask.options).toEqual(["I'm ready"]);
    expect(ask.question).toMatch(/sign in/i);

    expect(replay(readyAsked, readyAnswered).gates.asks).toEqual([]);
  });

  it("asks for free text when the run has no options to offer", () => {
    const asked = replay(fieldAsked);

    expect(asked.gates.asks[0].options).toEqual([]);
    expect(replay(fieldAsked, fieldAnswered).gates.asks).toEqual([]);
  });

  it("answers an ask that resolved before it arrived with no card at all", () => {
    const state = replay(fieldAnswered, fieldAsked);

    expect(state.gates.asks).toEqual([]);
    expect(state.gates.settled).toEqual([]);
  });

  it("keeps a card through the bootstrap that repairs a gap", () => {
    // The run is still blocked while this window re-bootstraps, and the
    // bootstrap has no cards in it: dropping them would hide the only
    // control that unblocks the run.
    const asked = replay(confirmAsked);
    const confirm: ConfirmView = asked.gates.confirms[0];
    const state = applyEnvelope(asked, {
      seq: 99,
      at: "1970-01-01T00:01:00Z",
      event: { type: "notice", level: "warning", code: "event_gap", text: "dropped 12 events" },
    });

    expect(state.sync.needsBootstrap).toBe(true);
    expect(state.gates.confirms).toEqual([confirm]);
  });

  it("lets one press through and refuses the second until the run answers", () => {
    const asked = replay(confirmAsked);
    const id = asked.gates.confirms[0].id;

    const claimed = claimGate(asked.gates, id);
    if (claimed === null) {
      throw new Error("the first press was refused");
    }
    // The second press of Approve, or Deny straight after it: the card is
    // already claimed and a second answer would reach a run that has
    // already been told what to do.
    expect(claimGate(claimed, id)).toBeNull();

    // The resolve is what releases it, so the claim cannot outlive the card.
    const resolved = applyEnvelope({ ...asked, gates: claimed }, confirmAnswered);
    expect(resolved.gates.pending).toEqual({});
  });

  it("makes the card answerable again when the command itself was refused", () => {
    const asked = replay(confirmAsked);
    const id = asked.gates.confirms[0].id;
    const claimed = claimGate(asked.gates, id);
    if (claimed === null) {
      throw new Error("the first press was refused");
    }

    expect(claimGate(releaseGate(claimed, id), id)).not.toBeNull();
  });
});
