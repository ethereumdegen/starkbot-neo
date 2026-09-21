import { beforeEach, describe, expect, it, vi } from "vitest";

import { initialState } from "./reduce";
import { useStore } from "./store";

/**
 * Closing a thread is announced back as a `ConversationReset`, and a reset of
 * the thread on screen means "read it again". For a thread that was just
 * deleted that would put an empty transcript under a dead id and send the
 * next message into it — so the screen has to leave the thread *before* the
 * command goes out, and the reset must then land on nobody.
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

const CONVERSATION = "0192f2cd-0000-7000-8000-00000000000a";
const OTHER = "0192f2cd-0000-7000-8000-00000000000b";
const row = (id: string, title: string) => ({
  id,
  title,
  created_at: 1_700_000_000_000,
  updated_at: 1_700_000_000_000,
});

beforeEach(() => {
  bridge.calls.length = 0;
  bridge.answers = {
    delete_conversation: null,
    list_conversations: [row(OTHER, "kept")],
    load_thread: [],
  };
});

describe("closing a thread", () => {
  it("leaves the active thread before deleting it, so the reset does not re-read a dead id", async () => {
    useStore.setState({
      ...initialState,
      conversation: {
        ...initialState.conversation,
        activeId: CONVERSATION,
        conversations: [row(CONVERSATION, "closing"), row(OTHER, "kept")],
        messages: [],
      },
    });

    const closing = useStore.getState().deleteConversation(CONVERSATION);
    // The reset arrives while the command is still in flight.
    useStore
      .getState()
      .applyEvent({
        seq: 1,
        at: "2026-01-01T00:00:01Z",
        event: { type: "conversation_reset", conversation_id: CONVERSATION },
      });
    await closing;

    const { conversation } = useStore.getState();
    expect(conversation.activeId).toBeNull();
    expect(conversation.threadStale).toBe(false);
    expect(bridge.calls.map((call) => call.cmd)).not.toContain("load_thread");
    expect(bridge.calls[0]).toMatchObject({ cmd: "delete_conversation", args: { id: CONVERSATION } });
    expect(conversation.conversations.map((entry) => entry.id)).toEqual([OTHER]);
  });

  it("keeps the thread on screen when a different one is closed", async () => {
    useStore.setState({
      ...initialState,
      conversation: {
        ...initialState.conversation,
        activeId: OTHER,
        conversations: [row(CONVERSATION, "closing"), row(OTHER, "kept")],
      },
    });

    await useStore.getState().deleteConversation(CONVERSATION);

    expect(useStore.getState().conversation.activeId).toBe(OTHER);
  });
});
