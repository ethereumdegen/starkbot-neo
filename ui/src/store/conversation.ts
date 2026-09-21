import type { AppEvent, ConversationId, ConversationView, MessageView } from "../bridge/api";

/**
 * The thread being shown, and the switcher beside it.
 *
 * Messages are held for the active conversation only: a window that cached
 * every thread it had ever opened would be holding a copy of the store that
 * nothing invalidates, and the store is one `load_thread` away.
 */
export interface ConversationState {
  conversations: ConversationView[];
  activeId: ConversationId | null;
  /** Oldest first, exactly as `load_thread` returns them. */
  messages: MessageView[];
  /** The active thread changed underneath us; re-read it. */
  threadStale: boolean;
  /** A thread was created or reset somewhere else; re-read the switcher. */
  listStale: boolean;
}

export const initialConversation: ConversationState = {
  conversations: [],
  activeId: null,
  messages: [],
  threadStale: false,
  listStale: false,
};

/**
 * `ConversationReset` is deliberately the same event for "a new thread" and
 * "this one was cleared" (runtime.rs): to a renderer both mean drop what you
 * have and read the thread again. Nothing is invented here from the id alone,
 * because a reset thread's first message may already have been appended.
 */
export function reduceConversation(state: ConversationState, event: AppEvent): ConversationState {
  switch (event.type) {
    case "message": {
      const { message } = event;
      if (message.conversation_id !== state.activeId) {
        // Another thread moved: the switcher's `updated_at` is now wrong, but
        // the thread on screen is not.
        return { ...state, listStale: true };
      }
      if (state.messages.some((row) => row.id === message.id)) {
        return state;
      }
      return { ...state, messages: [...state.messages, message] };
    }
    case "conversation_reset":
      return {
        ...state,
        listStale: true,
        threadStale: state.threadStale || event.conversation_id === state.activeId,
      };
    default:
      return state;
  }
}
