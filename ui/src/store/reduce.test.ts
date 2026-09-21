import { describe, expect, it } from "vitest";

import type { AppEvent, Envelope } from "../bridge/api";
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
});

describe("a run that ends badly", () => {
  it("moves running → failed and stops the clock", () => {
    clock = 1_700_000_000_000;
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [
        {
          type: "turn_failed",
          run: RUN,
          error: "the model returned no action",
          code: "agent_graph",
        },
        2,
      ],
    ]);

    const record = state.runs.byId[RUN];
    expect(record.status).toBe("failed");
    expect(record.error).toBe("the model returned no action");
    expect(record.endedAt).not.toBeNull();
    expect(elapsedMs(record, 9_999_999_999_999)).toBe(1000);
  });

  // The classification comes off the event, not off the sentence. A reword
  // on either side of the bridge used to turn every stopped run red.
  it("calls a run stopped when the code says so, whatever the sentence says", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [
        {
          type: "turn_failed",
          run: RUN,
          error: "the run was stopped after 3 steps",
          code: "cancelled",
        },
        2,
      ],
    ]);

    expect(state.runs.byId[RUN].status).toBe("cancelled");
    expect(state.runs.byId[RUN].code).toBe("cancelled");
  });

  it("does not call a run stopped because its sentence mentions cancelling", () => {
    const state = play([
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
      [
        {
          type: "turn_failed",
          run: RUN,
          error: "the vendor cancelled the subscription",
          code: "agent_request",
        },
        2,
      ],
    ]);

    expect(state.runs.byId[RUN].status).toBe("failed");
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
    // A navigator line is traced, but it opens no run record: nothing
    // publishes a terminal event for a run this window did not start, so
    // one seeded here would sit at `running` for the life of the window.
    expect(state.runs.byId[RUN]).toBeUndefined();
    expect(state.runs.order).toEqual([]);
  });

  it("keeps at most TRACE_CAP lines for one run", () => {
    const lines: [AppEvent, number][] = [];
    for (let step = 0; step < 320; step += 1) {
      lines.push([
        { type: "nav_step", run: RUN, step, line: `line ${step}`, kind: { kind: "outcome" } },
        step + 1,
      ]);
    }
    const entries = play(lines).trace.nav[RUN];

    expect(entries).toHaveLength(300);
    // The tail is what is kept: a long run's interesting part is its end.
    expect(entries[entries.length - 1].line).toBe("line 319");
    expect(entries[0].line).toBe("line 20");
  });
});

describe("the run list's bounds", () => {
  function chatRun(index: number): string {
    return `0192f2cd-0000-7000-8000-${String(index).padStart(12, "0")}`;
  }

  /**
   * The window is meant to stay open all day, so the list is capped — but a
   * *running* run is never the one dropped. It is the one the Stop button
   * has to be able to reach, and a run evicted while live would also never
   * be settled by its own terminal event.
   */
  it("drops the oldest settled runs and never a running one", () => {
    const events: [AppEvent, number][] = [];
    let seq = 1;
    // The first run is still going; the next sixty finish.
    events.push([{ type: "turn_started", run: chatRun(0), conversation: CONVERSATION }, seq++]);
    for (let index = 1; index <= 60; index += 1) {
      events.push([
        { type: "turn_started", run: chatRun(index), conversation: CONVERSATION },
        seq++,
      ]);
      events.push([
        {
          type: "turn_finished",
          run: chatRun(index),
          text: "done",
          steps: 1,
          exhausted: false,
          usage: null,
        },
        seq++,
      ]);
    }
    const runs = play(events).runs;

    expect(runs.order).toHaveLength(50);
    expect(Object.keys(runs.byId)).toHaveLength(50);
    expect(runs.order).toContain(chatRun(0));
    expect(runs.byId[chatRun(0)].status).toBe("running");
    // Newest first, and the oldest settled runs are the ones gone.
    expect(runs.order[0]).toBe(chatRun(60));
    expect(runs.byId[chatRun(1)]).toBeUndefined();
  });

  it("keeps at most TURN_CARD_CAP step cards for one turn", () => {
    const events: [AppEvent, number][] = [
      [{ type: "turn_started", run: RUN, conversation: CONVERSATION }, 1],
    ];
    for (let step = 0; step < 50; step += 1) {
      events.push([
        {
          type: "turn_step",
          run: RUN,
          step,
          thought: `thought ${step}`,
          action: { kind: "answer", target: null, goal: null, text: null },
        },
        step + 2,
      ]);
    }
    const steps = play(events).runs.byId[RUN].steps;

    expect(steps).toHaveLength(40);
    expect(steps[0].step).toBe(10);
    expect(steps[steps.length - 1].step).toBe(49);
  });
});
