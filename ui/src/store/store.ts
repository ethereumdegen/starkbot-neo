import { create } from "zustand";

import {
  api,
  errorOf,
  type AskId,
  type AxRequestView,
  type AxResponseView,
  type CaseListingView,
  type ConfirmId,
  type ConversationId,
  type Envelope,
  type Json,
  type KeyRow,
  type ModelRow,
  type RunId,
  type SettingsSection,
  type UiError,
} from "../bridge/api";
import { claimGate, releaseGate } from "./gates";
import { applyBootstrap, applyEnvelope, initialState, type AppState } from "./reduce";
import { currentChatRun, registerRun } from "./runs";

export type Screen =
  | "chat"
  | "runs"
  | "eval"
  | "projects"
  | "inspect"
  | "connections"
  | "settings";

export interface Banner {
  tone: "ok" | "warn" | "fail";
  text: string;
}

/** Reference data that comes from a command, not from the event stream. */
export interface Catalog {
  bridgeVersion: number;
  storePath: string;
  dataDir: string;
  evalCases: CaseListingView[];
  models: ModelRow[];
  /**
   * The three Keychain accounts and whether this machine needs them.
   *
   * Requiredness is only in the bootstrap — a `KeyStatus` event carries the
   * account and the state and nothing else — so the rows are kept here and
   * the live state is read from the health slice over the top of them.
   */
  keys: KeyRow[];
}

export interface UiState {
  screen: Screen;
  selectedRun: RunId | null;
  banner: Banner | null;
  /** False until the first bootstrap lands; the shell shows a placeholder. */
  ready: boolean;
  /** A command is in flight — used to disable the control that started it. */
  busy: boolean;
  /**
   * The handshake said this bundle and this binary speak different
   * protocols. Nothing else is worth rendering: every view model below is a
   * shape one of the two no longer agrees with, so the shell shows the
   * refusal and stops.
   */
  blocked: UiError | null;
}

interface Actions {
  bootstrap: () => Promise<void>;
  applyEvent: (envelope: Envelope) => void;
  setScreen: (screen: Screen) => void;
  selectRun: (run: RunId | null) => void;
  dismissBanner: () => void;
  selectConversation: (id: ConversationId) => Promise<void>;
  newConversation: (title?: string) => Promise<void>;
  renameConversation: (id: ConversationId, title: string) => Promise<void>;
  deleteConversation: (id: ConversationId) => Promise<void>;
  send: (text: string) => Promise<boolean>;
  stop: (run: RunId) => Promise<void>;
  /**
   * Answer a card. Pressing twice sends once: the second press finds the
   * card already claimed and does nothing at all.
   */
  resolveConfirm: (id: ConfirmId, outcome: "confirmed" | "denied") => Promise<void>;
  answerAsk: (id: AskId, answer: string) => Promise<void>;
  startNav: (form: NavForm) => Promise<void>;
  startApp: (app: string, goal: string) => Promise<void>;
  startEval: (filter: string | null, tags: string[], once: boolean) => Promise<void>;
  inspect: (request: AxRequestView) => Promise<AxResponseView | null>;
  saveSettings: (section: SettingsSection, value: Json) => Promise<void>;
  /** Re-read the case list: an app installed since launch changes it. */
  rescanCases: () => Promise<void>;
  /** Re-read settings without a full bootstrap, for when a tab is opened. */
  reloadSettings: () => Promise<void>;
}

export interface NavForm {
  url: string;
  goal: string;
  headless: boolean;
  profile: string;
  attach: string[];
  safety: boolean;
}

export type Store = AppState & { ui: UiState; catalog: Catalog } & Actions;

const emptyCatalog: Catalog = {
  bridgeVersion: 0,
  storePath: "",
  dataDir: "",
  evalCases: [],
  models: [],
  keys: [],
};

const THREAD_LIMIT = 500;
const CONVERSATION_LIMIT = 50;

/**
 * Error codes that mean "not now", not "broken".
 *
 * A deliberate stop, a suite that is already running, an empty selection —
 * painting those the same red as a store failure trains people to ignore
 * red, and the one that matters is the one they then miss.
 */
const REFUSALS: Record<string, true> = { cancelled: true, eval_busy: true, no_cases: true };

export const useStore = create<Store>()((set, get) => {
  /**
   * One place where a rejected command becomes something readable.
   *
   * Every command rejects with a `UiError` carrying a `fix`; swallowing it
   * would leave a button that silently does nothing, which is the failure
   * mode this app can least afford — the user cannot tell a refused action
   * from a slow one.
   */
  const guard = async <T,>(task: () => Promise<T>): Promise<T | null> => {
    set((state) => ({ ui: { ...state.ui, busy: true } }));
    try {
      return await task();
    } catch (thrown) {
      const error = errorOf(thrown);
      const tone = REFUSALS[error.code] === true ? "warn" : "fail";
      set((state) => ({ ui: { ...state.ui, banner: { tone, text: error.message } } }));
      return null;
    } finally {
      set((state) => ({ ui: { ...state.ui, busy: false } }));
    }
  };

  /**
   * Send one card's answer, at most once.
   *
   * A card is a run standing still, so the controls are the ones people hit
   * hardest — twice, or with Enter held down. The claim is taken before the
   * command leaves, and the second press finds it taken and returns: two
   * answers for one card would mean the run acted on whichever raced home.
   *
   * The card is not removed here. It goes when the run says it resolved,
   * which is the only moment the answer has actually landed — and when
   * another surface answered first, `no_such_card` says so in the words of
   * the thing that happened rather than as a failure of this window.
   */
  const answerCard = async (id: string, send: () => Promise<null>) => {
    const claimed = claimGate(get().gates, id);
    if (claimed === null) {
      return;
    }
    set({ gates: claimed, ui: { ...get().ui, busy: true } });
    try {
      await send();
    } catch (thrown) {
      const error = errorOf(thrown);
      const gone = error.code === "no_such_card";
      set((state) => ({
        // Gone means the resolve that clears this card is already on its
        // way; keeping the claim stops a second press chasing it.
        gates: gone ? state.gates : releaseGate(state.gates, id),
        ui: {
          ...state.ui,
          banner: gone
            ? { tone: "warn", text: "Something else answered that card first." }
            : { tone: "fail", text: error.message },
        },
      }));
    } finally {
      set((state) => ({ ui: { ...state.ui, busy: false } }));
    }
  };

  const reloadThread = async (id: ConversationId) => {
    const messages = await api.loadThread(id, THREAD_LIMIT);
    set((state) => ({
      conversation: { ...state.conversation, messages, activeId: id, threadStale: false },
    }));
  };

  const reloadList = async () => {
    const conversations = await api.listConversations(CONVERSATION_LIMIT);
    set((state) => ({ conversation: { ...state.conversation, conversations, listStale: false } }));
  };

  /**
   * The version check, made once per window and remembered.
   *
   * A gap in the event stream re-bootstraps, and re-asking the same question
   * on every repair would add a round trip to a path that is already
   * recovering. Two halves that disagree about the protocol cannot be talked
   * round at runtime, so the answer cannot change.
   */
  let agreed: Promise<boolean> | null = null;
  const handshake = () => {
    agreed ??= api
      .handshake()
      .then(() => true)
      .catch((thrown) => {
        set((state) => ({ ui: { ...state.ui, blocked: errorOf(thrown) } }));
        return false;
      });
    return agreed;
  };

  const bootstrap = async () => {
    if (!(await handshake())) {
      return;
    }
    await guard(async () => {
      const boot = await api.getBootstrap();
      set((state) => ({
        ...applyBootstrap(state, boot),
        catalog: {
          bridgeVersion: boot.bridge_version,
          storePath: boot.store_path,
          dataDir: boot.data_dir,
          evalCases: boot.eval_cases,
          models: boot.models,
          keys: boot.keys,
        },
        ui: { ...state.ui, ready: true },
      }));
    });
  };

  return {
    ...initialState,
    ui: {
      screen: "chat",
      selectedRun: null,
      banner: null,
      ready: false,
      busy: false,
      blocked: null,
    },
    catalog: emptyCatalog,

    bootstrap,

    /**
     * The single entry point from the event listener.
     *
     * A gap is repaired here rather than in the reducer because repairing it
     * is a command, and a reducer that called one would not be a reducer.
     */
    applyEvent: (envelope) => {
      const before = get();
      const next = applyEnvelope(before, envelope);
      if (next === before) {
        return;
      }
      set(next);
      if (next.sync.needsBootstrap) {
        void bootstrap();
        return;
      }
      if (next.conversation.threadStale && next.conversation.activeId !== null) {
        void guard(() => reloadThread(next.conversation.activeId as ConversationId));
      }
      if (next.conversation.listStale) {
        void guard(reloadList);
      }
    },

    setScreen: (screen) => set((state) => ({ ui: { ...state.ui, screen } })),
    selectRun: (run) => set((state) => ({ ui: { ...state.ui, selectedRun: run } })),
    dismissBanner: () => set((state) => ({ ui: { ...state.ui, banner: null } })),

    selectConversation: async (id) => {
      await guard(() => reloadThread(id));
    },

    newConversation: async (title) => {
      await guard(async () => {
        const created = await api.newConversation(title);
        await reloadList();
        await reloadThread(created.id);
      });
    },

    renameConversation: async (id, title) => {
      await guard(async () => {
        await api.renameConversation(id, title);
        await reloadList();
      });
    },

    /**
     * Close a thread. The screen leaves it *before* the command goes out:
     * the delete is announced as a `ConversationReset`, and a reset of the
     * active thread is an instruction to re-read it — which for a thread
     * that no longer exists would put an empty transcript under a dead id,
     * and the next message typed would be sent into it. Leaving first means
     * the reset arrives for a thread nobody is looking at, and only the
     * switcher is marked stale.
     */
    deleteConversation: async (id) => {
      set((state) =>
        state.conversation.activeId === id
          ? {
              conversation: {
                ...state.conversation,
                activeId: null,
                messages: [],
                threadStale: false,
              },
            }
          : state,
      );
      await guard(async () => {
        await api.deleteConversation(id);
        await reloadList();
      });
    },

    /**
     * What was typed: a new turn, or a word into the one already running.
     *
     * A second concurrent chat run in one thread is never started. Two turns
     * reading and writing the same transcript would each answer a history
     * the other was still editing, and the thread would read as one
     * conversation held by two agents that cannot hear each other. The
     * running turn takes the message at its next step boundary instead, and
     * the backend writes it into the thread as it arrives — so nothing is
     * painted here optimistically and nothing can be shown that the turn
     * never received.
     */
    send: async (text) => {
      return (await guard(async () => {
        // Typing into an empty window used to answer "Start a conversation
        // first" — the thread is bookkeeping and the message is the intent,
        // so the first message opens one instead of being refused.
        let conversation = get().conversation.activeId;
        if (conversation === null) {
          const created = await api.newConversation();
          await reloadList();
          await reloadThread(created.id);
          conversation = created.id;
        }
        const live = currentChatRun(get().runs, conversation);
        // `false` is the race, not a refusal: the turn ended between the
        // keystroke and the command, so what was typed becomes a new turn.
        if (live !== null && live.status === "running" && (await api.steerRun(live.run, text))) {
          return true;
        }
        const run = await api.sendMessage(conversation, text);
        set((state) => ({
          runs: registerRun(state.runs, run, "chat", Date.now(), conversation),
          ui: { ...state.ui, selectedRun: run },
        }));
        return true;
      })) === true;
    },

    /**
     * Stopping cancels the run's token. It cannot reclaim a Chrome window
     * that is already open, so the banner says what actually happened rather
     * than implying the machine went back to how it was.
     */
    stop: async (run) => {
      await guard(async () => {
        const live = await api.stopRun(run);
        set((state) => ({
          ui: {
            ...state.ui,
            banner: live
              ? { tone: "ok", text: "Stop sent. A browser this run opened stays open." }
              : { tone: "warn", text: "That run had already finished." },
          },
        }));
      });
    },

    resolveConfirm: (id, outcome) => answerCard(id, () => api.resolveConfirm(id, outcome)),

    answerAsk: (id, answer) => answerCard(id, () => api.answerAsk(id, answer)),

    startNav: async (form) => {
      await guard(async () => {
        const run = await api.runNav(
          form.url,
          form.goal,
          form.headless,
          form.profile.trim() === "" ? null : form.profile.trim(),
          form.attach,
          form.safety,
        );
        set((state) => ({
          runs: registerRun(state.runs, run, "nav", Date.now()),
          ui: { ...state.ui, selectedRun: run, screen: "runs" },
        }));
      });
    },

    startApp: async (app, goal) => {
      await guard(async () => {
        const run = await api.runAppGoal(app, goal);
        set((state) => ({
          runs: registerRun(state.runs, run, "app", Date.now()),
          ui: { ...state.ui, selectedRun: run, screen: "runs" },
        }));
      });
    },

    /**
     * Eval runs one case at a time by design — the cases share the keyboard
     * and the frontmost app. The backend refuses a second one, and this
     * refuses to ask: a disabled button explains itself, a rejected command
     * arrives as an error the user did not cause.
     */
    startEval: async (filter, tags, once) => {
      const running = get().runs.order.some((id) => {
        const record = get().runs.byId[id];
        return record.kind === "eval" && record.status === "running";
      });
      if (running) {
        set((state) => ({
          ui: {
            ...state.ui,
            banner: { tone: "warn", text: "An eval suite is already running." },
          },
        }));
        return;
      }
      await guard(async () => {
        const run = await api.runEval(filter, tags, once);
        set((state) => ({
          runs: registerRun(state.runs, run, "eval", Date.now()),
          ui: { ...state.ui, selectedRun: run },
        }));
      });
    },

    inspect: (request) => guard(() => api.runAx(request)),

    saveSettings: async (section, value) => {
      await guard(async () => {
        const settings = await api.patchSettings(section, value);
        set((state) => ({
          settings: { settings, revision: state.settings.revision + 1 },
          ui: { ...state.ui, banner: { tone: "ok", text: `Saved ${section}.` } },
        }));
      });
    },

    rescanCases: async () => {
      await guard(async () => {
        const evalCases = await api.listEvalCases();
        set((state) => ({ catalog: { ...state.catalog, evalCases } }));
      });
    },

    /**
     * Settings arrive in the bootstrap and every change publishes
     * `SettingsChanged`, so this is not the usual path — it is the repair for
     * a window that was open while something else edited the store.
     */
    reloadSettings: async () => {
      await guard(async () => {
        const settings = await api.getSettings();
        set((state) => ({ settings: { settings, revision: state.settings.revision + 1 } }));
      });
    },
  };
});
