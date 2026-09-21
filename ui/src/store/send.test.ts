import { beforeEach, describe, expect, it, vi } from "vitest";

import { initialState } from "./reduce";
import { registerRun } from "./runs";
import { useStore } from "./store";

/**
 * What the composer does is a decision, not a render: one message either
 * goes into the turn that is running or starts a new one, and getting that
 * wrong puts two agents in one thread, each answering a history the other is
 * still editing. The commands are recorded rather than sent — there is no
 * backend here, and the assertion is about which one was chosen.
 */
const bridge = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> }[],
  answers: {} as Record<string, unknown>,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args: Record<string, unknown>) => {
    bridge.calls.push({ cmd, args });
    return Promise.resolve(bridge.answers[cmd]);
  },
}));

const RUN = "0192f2cd-0000-7000-8000-000000000001";
const NEXT_RUN = "0192f2cd-0000-7000-8000-000000000002";
const CONVERSATION = "0192f2cd-0000-7000-8000-00000000000a";

/** A thread with one chat turn already running in it. */
function live() {
  useStore.setState({
    ...initialState,
    runs: registerRun(initialState.runs, RUN, "chat", 1_700_000_000_000, CONVERSATION),
    conversation: { ...initialState.conversation, activeId: CONVERSATION },
  });
}

beforeEach(() => {
  bridge.calls.length = 0;
  bridge.answers = { steer_run: true, send_message: NEXT_RUN };
});

describe("typing while a turn is running", () => {
  it("steers the turn instead of starting a second one", async () => {
    live();

    await useStore.getState().send("per seat, not per org");

    expect(bridge.calls.map((call) => call.cmd)).toEqual(["steer_run"]);
    expect(bridge.calls[0].args).toMatchObject({ run: RUN, text: "per seat, not per org" });
    // The steered message comes back as a stored row on the event stream;
    // nothing is painted here on the strength of having asked.
    expect(useStore.getState().conversation.messages).toEqual([]);
  });

  it("sends it as a new turn when the run ended between the keystroke and the command", async () => {
    live();
    bridge.answers.steer_run = false;

    await useStore.getState().send("and in euros");

    expect(bridge.calls.map((call) => call.cmd)).toEqual(["steer_run", "send_message"]);
    expect(useStore.getState().runs.byId[NEXT_RUN].conversation).toBe(CONVERSATION);
  });

  it("starts a turn normally when the last one has finished", async () => {
    live();
    useStore.setState((state) => ({
      runs: {
        order: state.runs.order,
        byId: { ...state.runs.byId, [RUN]: { ...state.runs.byId[RUN], status: "finished" } },
      },
    }));

    await useStore.getState().send("what about storage");

    expect(bridge.calls.map((call) => call.cmd)).toEqual(["send_message"]);
  });
});
