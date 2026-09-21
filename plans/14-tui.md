# 14 — TUI: the terminal harness (`neo-tui`)

Dated 2026-09-19. **P12**: two front ends over one core. `neo tui` is a ratatui + crossterm terminal harness modelled on the OMP coding harness — one full-width page at a time (Conversation, Runs, Mind/trace) plus confirm cards (P15) — and it is the **default surface for development and for every milestone's acceptance**: a milestone is done when it works in the TUI; the desktop UI follows. [04-ui](04-ui.md) owns the Tauri app, the visual language and the typed event/command vocabulary; this doc renders that same vocabulary in a terminal and invents nothing parallel to it. Where a noun appears in both docs (`ListenState`, message, task, trace item, confirm card, Steer ticker, `patch_settings`, `SettingsChanged`) it is the same noun, the same variant set and the same wire type.

Owned elsewhere and only referenced here: events, commands and channels (04 §14), trace-item and confirm vocabulary (03 §2, §5, §6), the `Runtime` facade and the crate map (05 §1), runtimes and capability flags (08), safety heads and rules (03 §2, 10).

## 1. What it is

- **Crate `neo-tui`**: a library plus the `neo tui` subcommand in `neo-cli` (`neo-cli ─▶ neo-tui ─▶ neo-agent`). Tauri-free like everything below `src-tauri` (A1), so the TUI runs wherever the headless `neo` binary runs. Dependencies: `ratatui` 0.30 + `crossterm` 0.29 (pinned in 05 §3, which owns the workspace entry), `tokio`, `neo-agent`, `neo-core`. No new protocol, no new state store, no serialization: it subscribes to `neo_agent::Runtime` in process (§4).
- **Modelled on the OMP coding harness** in shape only — a full-screen alternate-screen layout with a conversation pane, a work queue, a reasoning/trace pane, a modal composer and inline approval cards. It is **not** a coding harness: Starkbot is a GTM marketing and media harness (P2′) and not a shell/coding agent (P3). The TUI has no shell, no file browser, no arbitrary-command palette (§5).
- **Thin, like the webview.** It holds **no secrets and no logic**: it renders state pushed from the core and sends typed commands (P12). Every decision — intake, routing, gating, spend, pausing — happens in `neo-judge`/`neo-agent` exactly as it does for the desktop app.
- **Voice still works here.** `neo tui` drives the same `neo-voice` stack through the core; the mic, VAD, STT and TTS are unchanged (P4). The terminal only lacks the desktop's panels (pill, ring, quick entry) and the tray, which are `src-tauri`'s and are simply absent.
- **Feature parity is defined by events, not by widgets.** Any `AppEvent` variant the core can emit must be renderable in the TUI; a variant with no TUI rendering is a bug in this doc, not a missing feature in the core.

## 2. Layout

One full-screen view: a header line, a pane row, the composer, and the status line. Overlays (confirm, ask, help, settings, detail) draw over the pane row and never over the status line.

| Region | Maps to (04) | Content |
|---|---|---|
| Header | listening bar (§5) | name · `ListenState` glyph + label · level meter (ASCII bar, ≤ 10 Hz) · input device · `MUTED`/`PAUSED` badges |
| Conversation pane | Assist · Conversation (§6) | messages with source glyph (`◉` heard · `⌨` typed) and **intake tag** (`→ task · navigate`, `→ amend`, `ignored 0.12`, `enqueue? 0.55`); bot turns, quick-reply chips, streamed replies; `> ` composer below it |
| Queue pane | Assist · Queue (§7) | one `TaskCard` line-block per task in its state (`queued · running · needs_confirm · waiting_user · done · failed · cancelled`), running card expanded with the step sentence, steps · elapsed · spend vs cap; lane tag for tasks running in parallel (A10); paused-reason banner at the top |
| Mind pane | Assist · Mind (§8) | the trace timeline for the selected task — `Route · Thought · Steer · TextHelper · Action · Judgment · Saw · Said · Asked · Amended · Gate · Media · Pack` (03, 04 §14); auto-follows the newest item unless scrolled up (a `▼ live` indicator, `f` to re-follow) |
| Status line | status strip (§9) | mode · listen state · queue state · runtime + model · spend today · Jev/inference health · step latency · permissions |
| Confirm / ask overlay | confirm + ask cards (§7) | centred modal, §3 keys only |
| Settings view | Settings (§12) | full-screen row list replacing the pane row |

```
┌ Stark  ● LISTENING  ▁▂▅▇▅▂▁  MacBook Pro Microphone ───────────────────────────────────┐
│ Conversation ─────────────────┬ Queue (3) ─────────────┬ Mind · task 3f9a · navigate ──┤
│ ◉ post the launch note on x   │ ▶ running · navigate   │ Route   new_task 0.94 → navi… │
│   → task · navigate           │   post the launch no…  │ Steer   CLICK [12] "Compose"  │
│ ⌨ use the 9:16 cut            │   step 7 · 0:24 · $0.03│         0.97 · 164 ms         │
│   → amend                     │ ○ queued · multi       │ Steer   TYPE_TEXT [31] …      │
│ ⬤ Stark: posted, here's the…  │   pull last week's ad… │ Gate    needs_confirm: rule   │
│                               │ ✓ done · question      │         must-confirm "Post"   │
│                               │   what did you spend…  │                        ▼ live │
├───────────────────────────────┴────────────────────────┴───────────────────────────────┤
│ > …or type                                                                    [2 files]│
├────────────────────────────────────────────────────────────────────────────────────────┤
│ NORMAL │ ● LISTENING │ 3 queued·running │ API · gpt-5.6-sol │ $0.42/$10.00 │ Jev 168ms │
└────────────────────────────────────────────────────────────────────────────────────────┘

confirm overlay (drawn over the pane row; the status line stays visible)
        ┌ CONFIRM · rule: must-confirm label "Post" ────────────────────────┐
        │  Click Post on x.com                                              │
        │  x.com · "Compose post" · button "Post" · near: "Draft saved"     │
        │  outward   ███████░░░ 0.71 ▲0.40     destructive ██░░░░░░░░ 0.18  │
        │  spends    ░░░░░░░░░░ 0.03 ▲0.40     on_task     █████████░ 0.93  │
        │  task $0.03 / $1.00 · today $0.42 / $10.00              1:47 left │
        │  [y] approve   [n] deny   [r] always allow here   [s] show me     │
        └───────────────────────────────────────────────────────────────────┘
```

**Widths (P15, supersedes the three-pane layout).** **One page at a time, full width.** The Conversation page is drawn borderless so the whole terminal width goes to the transcript; Runs and Mind keep a titled frame, because their titles carry counts. Pages are reached by number (`1`/`2`/`3`), `Tab`/`BackTab`, or `<`/`>` — which move *between* pages rather than resizing a split, since there is nothing to resize — and the page strip names each page with its key. Three columns were the original design and were wrong here: the Conversation is the surface a user reads, and at 80 columns a third of the width re-wraps a sentence every four words while two columns sit mostly empty. **Heights.** < 24 rows: the header collapses into the status line. The minimum is **60 × 20**; below it the TUI renders one centred line, "terminal too small — 60×20 minimum", and keeps consuming events so nothing is lost on resize.

**Colour.** The 04 §2 state colours map to the 16-colour ANSI palette (listen → cyan, working → blue, confirm → yellow, danger → red, ok → green, muted → grey) and every state also carries a word and a glyph, so `NO_COLOR`, a 2-colour terminal and a colour-blind reader all still read the state. Unicode glyphs degrade to ASCII when the terminal reports no UTF-8 locale.

## 3. Input model

Voice is still the primary input (P4) and the TUI shows the same heard-utterance thread. But the TUI **must be fully usable typed-only**: every action reachable by voice or by mouse in the desktop app is reachable by keyboard here, and a ChatGPT-only or Anthropic-only user with no OpenAI key (K6) gets a fully working harness with the mic never opened. Typed input goes through the same **intake → queue → gates** path (P4) — the composer calls `send_message` and the core classifies it; the TUI never enqueues a task itself.

Modal, vim-ish: **NORMAL** (keys are commands), **INSERT** (the composer owns the keyboard), **CARD** (a confirm or ask overlay owns the keyboard), **COMMAND** (`:` line), **SEARCH** (`/` line). The current mode is the first segment of the status line. Everything is rebindable in Settings → Shortcuts (`set_shortcut`), sharing action ids with the desktop app wherever the action exists in both.

| Scope | Key | Action | Command (04 §14) |
|---|---|---|---|
| Any mode | `Ctrl-C` | **kill switch** — stop the running task, deny pending confirms, stop TTS (03 §5.7) | `kill_switch` |
| Any mode | `Ctrl-Q` | quit the TUI (asks while a task is running; the core keeps running if the app owns it) | — |
| Any mode | `Ctrl-L` | force redraw | — |
| Any mode | `?` | help overlay: the live keymap | — |
| Normal | `1` `2` `3` | focus Conversation · Queue · Mind | — |
| Normal | `Tab` / `Shift-Tab` | next / previous pane | — |
| Normal | `<` `>` | shrink / grow the focused split | — |
| Normal | `i` / `a` | enter INSERT (focus composer) | — |
| Normal | `:` / `/` | COMMAND line · search the thread | `search_messages` |
| Normal | `f` | toggle follow-live for the focused pane (Conversation autoscroll, Mind auto-follow) | — |
| Normal | `p` | pause / resume the queue | `pause_queue(on)` |
| Normal | `x` | stop the current task | `stop_current` |
| Normal | `m` | mute / unmute listening | `set_listen(on)` |
| Normal | `o` / `O` | open Stark's Chrome · bring the bot's tab forward | `open_managed_chrome` · `show_bot_tab` |
| Normal | `Ctrl-N` | new conversation (resets the digest, not the history) | `new_conversation` |
| Normal | `,` | Settings view | `get_settings` |
| Normal | `q` | close the overlay / leave the view; at top level, ask to quit | — |
| Lists | `j` `k` / `↓` `↑` | move the selection | — |
| Lists | `Ctrl-D` `Ctrl-U` / `Ctrl-F` `Ctrl-B` | half page · page | — |
| Lists | `gg` / `G` | first · last item | — |
| Lists | `Space` / `h` `l` | expand / collapse (trace item, Steer line, task card) | — |
| Lists | `Enter` | open detail (full Jev state, full observation, raw JSON) | `get_trace_item_detail` |
| Conversation | `Enter` on `enqueue?` / `ignored` | accept the chip · force-enqueue | `accept_enqueue` · `force_enqueue` |
| Conversation | `Enter` on a turn with a task | select that task in Queue and Mind | `get_trace` |
| Conversation | `n` / `N` | next / previous search match | — |
| Queue | `Enter` | select the task (Mind follows) | `get_trace` |
| Queue | `J` / `K` | reorder a queued task down · up | `reorder_task` |
| Queue | `e` | edit a queued task's text (opens INSERT on that row) | `edit_task` |
| Queue | `D` | delete a queued task | `delete_task` |
| Queue | `r` | retry a failed task | `retry_task` |
| Mind | `[` / `]` / `t` | previous · next task · task picker | `get_trace` |
| Mind | `F` | cycle the item-kind filter chips | — |
| Mind | `Y` | copy the trace, redacted, to the system clipboard | `copy_trace_redacted` |
| Insert | `Enter` | send — or answer, when the composer is retargeted by a pending ask | `send_message` · `answer_ask` |
| Insert | `Alt-Enter` / `Ctrl-J` | newline (`Shift-Enter` only where the terminal reports it *(verify per terminal)*) | — |
| Insert | `↑` on an empty composer | edit the last typed message while its task is still `queued` | `edit_queued_message` |
| Insert | `Ctrl-W` / `Ctrl-U` | delete word · line | — |
| Insert | `Ctrl-A` | attach: a one-line path prompt; the core stages the file and returns only a chip (P3: media import, not a file browser — no listing, no completion, no read-back) | `stage_attachment(path)` |
| Insert | `Esc` | leave INSERT, keep the draft | — |
| **Card** | `y` | **approve** the confirm | `resolve_confirm(id, true, remember)` |
| **Card** | `n` | **deny** the confirm | `resolve_confirm(id, false, false)` |
| **Card** | `r` | toggle "always allow this here" (absent for spend, media cost and pack-policy must-confirms) | — |
| **Card** | `s` | show me — flash the target, bring the managed Chrome window forward | `show_bot_tab` |
| **Card** | `j` `k` | scroll the card's context and head bars | — |
| **Card** | `1`–`9` | pick a quick-reply option on an **ask** card | `answer_ask` |
| **Card** | `i` | type a free-text answer to an ask card | `answer_ask` |
| **Card** | `Esc` | unfocus — the card stays pending, its countdown keeps running | — |
| Command | `Tab` | complete from the closed command list (§5) | — |
| Command | `Enter` / `Esc` | run · cancel | — |
| Settings | `Enter` / `Space` | toggle or edit the row (numeric rows prompt and preview) | `patch_settings` · `preview_threshold` |
| Settings | `Esc` / `q` | back to the pane row | — |

Rules that the keymap may never break:

- **Every destructive or spending action still goes through the confirm card.** The TUI adds **no** approval shortcut that resolves a confirm without the action sentence on screen (04 §13's rule, restated for the terminal): `y`/`n` are live **only** while the card is focused, fully rendered, and has been visible for ≥ 600 ms — the same debounce the voice `yes`/`no` vocabulary uses (03 §2.1). A keystroke arriving inside that window is dropped, not queued. `Enter` never resolves a card, so a stray composer `Enter` cannot approve anything.
- **No bulk approval.** One card, one decision. There is no "approve all", no `--yes`, no setting that pre-approves a class of action outside Settings → Safety, and `r` (remember) is hidden exactly where 04 §7 hides the checkbox.
- **A card raised while INSERT is active** does not steal the keyboard mid-word: the mode indicator flips to `CARD ↩` and the pane row dims, but the first keystroke after the card appears still goes to the composer if it is a printable character. `Esc` then `y` is always the explicit path; the desktop app's countdown and the voice path stay live throughout.
- **The kill switch is always reachable**, in every mode, including inside a modal (03 §5.7). In raw mode crossterm delivers `Ctrl-C` as a key event rather than SIGINT, so the TUI must map it deliberately; a second `Ctrl-C` within 2 s after the kill switch has already fired quits.

## 4. How it talks to the core

One seam, shared by both front ends: **`neo_agent::Runtime`** — the facade 05 §1 already defines as `start`, commands in, `AppEvent` stream out.

| Concern | Desktop (04) | TUI |
|---|---|---|
| Bootstrap | `get_bootstrap()` over the Tauri bridge | `Runtime::bootstrap()` — the same `Bootstrap` struct, no JSON round-trip |
| Events | `AppEvent` fanned out to every window by `tauri-specta` | `Runtime::subscribe() -> impl Stream<Item = AppEvent>`, one subscription per process |
| High-rate data | `Channel<T>` (`mic_levels`, `partial_transcripts`, `text_deltas`, `steer_ticks`) | the same four channels as in-process receivers, opened by the same `open_*` commands |
| Commands | `invoke("send_message", …) -> Result<T, UiError>` | `Runtime::command(Command::SendMessage { … }) -> Result<_, UiError>`; the same `UiError { code, message, fix }` |

- The Tauri bridge is a **transport over this facade, not a second API**. `src-tauri` serializes `Command`/`AppEvent`; `neo-tui` passes them by value. Adding a command or an event variant is one change in `neo-core`/`neo-agent`, and both front ends see it. A command that exists only for one front end is not allowed; window-management commands (`set_mode`, `pop_out_design`, `show_main`, `set_pill_position`) are the one exception and the TUI simply never sends them.
- **Rule: no front end may hold state the core does not.** Everything rendered arrives from `bootstrap` + `AppEvent`. The TUI's own state is an enumerated, throwaway list — focus, scroll offsets, expansion set, follow-live flags, split ratios, the current mode, the composer draft, the filter chips — and none of it changes what the agent does. If a pane needs a fact that is not in an event, the fix is a field in the core, never a TUI-local computation.
- **No optimistic updates.** A keystroke sends a command; the row changes when the resulting `AppEvent` arrives. The pressed control renders a `…` pending marker until then. This is what keeps the two front ends from diverging and is what makes the TUI a valid acceptance surface for the desktop app's behaviour.
- **Ordering and gaps.** Events carry the monotonic `seq` of 04 §14; a gap re-bootstraps and redraws. The render loop is event-driven with a 60 ms coalescing tick: a burst of `Trace`/`steer_ticks` produces **one** frame, and the Steer ticker renders ≤ 10 lines/s whatever arrives (04 §15's budget, restated for the terminal). Idle CPU with Listen on: < 1 % of one core, and exactly zero frames when nothing changes.
- **Reducer seam.** `(TuiState, AppEvent) -> TuiState` and `(TuiState, KeyEvent) -> (TuiState, Vec<Command>)` are pure functions, mirroring the desktop store's pure reducers. They are the unit-test surface (§6); the terminal, the clock and the `Runtime` are behind traits.

## 5. Secrets and safety

- **The TUI never renders a secret and never echoes a key.** It receives `KeyStatus { account, status: present | invalid | missing }` and `ProviderAccount` (redacted) and nothing else — the same rule the webview lives under (K1, K6). Key material never reaches `neo-tui`; only `neo-keys` holds it (05 §1 rule 2).
- **Entering a key** uses one masked prompt in Settings → Connections & keys: paste-only, echoes `•`, never re-displays, never pre-fills, sends `set_key(account, key)` which returns only a `KeyState`. It is the **only** place the TUI accepts secret input — never the composer, never the `:` line, never an argument. A pasted string matching a key pattern in the composer is refused with a pointer to Settings → Keys (04 §6).
- **Nothing secret can reach the scrollback.** The TUI runs in the alternate screen in raw mode and writes to stdout only through ratatui's buffer; `print_stdout` stays **denied** for `neo-tui` (05 §2 grants that opt-out to `neo-cli` only), logs go to the `tracing` file appender, never to the terminal, and the redaction rules of `neo-keys` apply to every string the TUI renders, including `Notice` text and Doctor output. On quit or panic the alternate screen is torn down in a guard so a panic message cannot leave a half-redacted frame behind.
- **No shell, no file browser, no arbitrary command palette (P3).** The `:` line is a **closed, static list** — `:settings :keys :models :packs :doctor :soul :trace <task> :new :pause :resume :listen on|off :kill :help :quit` — parsed into typed `Command`s. It takes no free-form arguments beyond ids and enum values, it cannot run a program, it cannot open a path, and unknown input is rejected with "unknown command", never forwarded anywhere. There is no `!` escape, no `:e <file>`, no directory listing and no shell completion. `:soul` edits `soul.md` in an in-app buffer or hands off through the core's `open_soul_external` (P8: preferences, not permissions — the same notice as 04 §10 is pinned above the buffer); `neo-tui` itself spawns no process.
- **Screen lock (P7).** On lock or display sleep the core pauses the queue and listening and fails the running desktop task as `screen locked`; the TUI shows the same banner text as the desktop app — header `PAUSED: SCREEN LOCKED`, the queue's paused-reason banner, and the `failed: screen locked` card — and clears it by itself on unlock. The TUI has no way to override it.
- **Safety is the core's (A9).** Rules, heads and thresholds are evaluated in `neo-judge`; the TUI renders the cause, the four head bars with their threshold ticks and the cost estimate, and sends one `resolve_confirm`. A Jev outage shows the same "confirming everything until Jev is back" banner and fails closed, unchanged.
- **Read-only where the desktop app is read-only.** The TUI cannot loosen a safety setting from outside Settings → Safety, cannot approve from a notification (it has none), and cannot answer another app's permission prompt (A21).

## 6. Test plan

Rust only — no Python anywhere in this project (A1, 05 §13). The corpus is the one that already exists: the `AppEvent` / `TraceItem` sample JSON written by the `neo-core` parity test for 04 §17, so one fixture set drives both front ends.

| Layer | How |
|---|---|
| **Render goldens** | `ratatui::backend::TestBackend` at 120×40, 100×30, 80×24 and 60×20. A scripted event stream (the fixture corpus, replayed deterministically with a fake clock) is drawn, the `Buffer` is dumped to text with a style legend, and compared against `fixtures/tui/*.txt`. Coverage: every `ListenState`, every `TaskStatus`, every paused reason, every `TraceItem` kind, every intake tag, confirm cards with and without cost and with and without "always allow", ask cards, the too-small screen, the four widths |
| **Golden regeneration** | `neo dev tui-goldens` rewrites the files; CI runs the comparison and fails on drift, exactly like the bindings drift check |
| **Key handling** | unit tests over the pure `(TuiState, KeyEvent) -> (TuiState, Vec<Command>)` reducer: focus cycling and list motion at every width, follow-live on/off, composer editing and `↑` recall, `:`-line parsing (every valid command maps to one typed `Command`; unknown input produces none), the confirm arming delay (a `y` before 600 ms emits nothing), `Esc` never emitting `resolve_confirm`, `Enter` never resolving a card, `Ctrl-C` emitting `kill_switch` from every mode including inside a modal |
| **Event handling** | the shared corpus through the event reducer: intake-tag lifecycle, confirm resolved by card vs voice vs timeout, `seq` gap → re-bootstrap, paused-reason stacking, trace paging merged with live items |
| **Secrets** | a fixture key string is put in the Keychain and in a `Notice`; no rendered cell in any golden contains it; the masked prompt's buffer never reaches the frame; a compile-level check that `neo-tui` has no `print!`/`println!` (clippy `print_stdout` stays denied for this crate) |
| **Manual smoke** (each milestone, recorded in the milestone's checklist) | in Terminal.app: `neo tui` → type a task → watch the intake tag, the queue card and the Steer ticker → approve a confirm with `y` and check the action sentence was on screen → answer an `ask_user` with `1` → `p` pause and resume → lock the screen and watch the banner and the auto-resume (P7) → `Ctrl-C` kill switch mid-run → resize to 100, 80 and 60 columns while a task runs → `Ctrl-Q`. With Listen on, repeat the first two steps by voice |

## 7. Milestones and acceptance

**P12's rule: a milestone is done when it works in the TUI**; the desktop UI follows. Each row below is the TUI half of the milestone's acceptance and is checked before the same milestone's 04 §18 row.

| M | The TUI shows / does | Done when |
|---|---|---|
| **M1 Shell** | boots on the `Runtime` facade; bootstrap render; header + empty panes + status line; Settings view with Connections & keys, Models, Doctor; the masked key prompt; the inference-connection picker for all four paths (K6) and the typed-only notice when no OpenAI key is present; `neo doctor` output in a pane | a fresh install reaches a validated TypeSafe + one-inference-connection state entirely from the terminal; no key string appears in any rendered buffer (test); goldens exist for every M1 state |
| **M2 Ears + Conversation** | all eight `ListenState` values in the header with label and glyph; the ASCII level meter off the `mic_levels` channel; Conversation thread with source glyphs, partial transcripts, attachments chips; the composer in INSERT with send, newline, `↑` recall and search | the header state always equals the core's `ListenState` and follows the mic within 1 s (the honesty check, run in the terminal too); typed and heard messages persist, page and search; the level meter costs ≤ 1 frame per 100 ms and zero frames when silent |
| **M3 Navigator (web)** | Mind pane live: `Route`, `Steer` ticker with operation, target, probability and latency, `Saw` diffs, expansion to the distributions and the four heads; the managed-Chrome presence line; `o`/`O` | a full `navigate` run is legible end to end in the terminal, with the ticker sustaining 10 lines/s inside one 60 ms frame budget; a `BLOCKED: login wall` renders its `waiting_user` card with the sign-in sentence |
| **M4 Judge, queue, safety** | intake tags with route, `ignored`/`enqueue?` chips and their keys; Queue with every card state and every paused reason; the confirm overlay with cause, action sentence, context, four head bars and cost; the ask overlay with numbered options; kill switch; trace paging from SQLite | a must-confirm action parks with the right cause and bars and resolves only from the card; `y` before 600 ms does nothing; the timeout denies; "always allow" is absent on spend confirms; lock → banner → `failed: screen locked` → auto-resume; the Jev-outage banner appears and everything confirms; `Ctrl-C` stops a run in < 50 ms to first effect |
| **M5 Sol orchestrator** | streamed `Thought` and reply text off `text_deltas`; the conversation digest indicator; `ask_user` exchanges answered by chip or free text; `Action`, `Judgment`, `Gate` and `Pack` items; the `soul.md` buffer with its preferences-not-permissions notice (P8); parallel read-only tasks shown with their lane tag (A10) | a multi-stage Sol task runs to `done` from the terminal with every tool call visible as a trace item; an `ask_user` answered with `1` resolves in both the card and the thread; the runtime + model segment is correct for whichever of the four runtimes is active (K6, A22) and never shows a token or an allowance converted to dollars |

Later milestones reuse these panes without new TUI surfaces: M6′ media-app runs are ordinary `navigate` traces with `Media` items; M9 adds a Packs view to Settings; M11 adds AX trace items; M12 adds the `SPEAKING` state and voice confirms.

## 8. Risks

| Risk | Mitigation |
|---|---|
| The two front ends drift — a fix lands in one and not the other | one `Runtime` facade, one `Command`/`AppEvent` set, one fixture corpus driving both test suites; a command that exists for only one front end is not allowed (§4) |
| A terminal key shortcut resolves a confirm the user did not read | `y`/`n` live only in CARD scope, only while the sentence is rendered, only after 600 ms; `Enter` never resolves; no bulk approval; the same rule as 04 §13 |
| Terminals disagree about `Shift-Enter`, `Alt-Enter`, bracketed paste and mouse reporting | `Ctrl-J` is the always-available newline; keys are rebindable; the help overlay shows what the current terminal actually reports; capability differences are detected at start, not assumed *(verify per terminal in the M2 smoke)* |
| A secret leaks into scrollback through a panic, a log line or a crash dump | alternate-screen teardown guard on panic and on exit; `print_stdout` denied for `neo-tui`; logs to the file appender only; a golden test asserts no fixture key string reaches any cell |
| The TUI becomes a shell by accident (a `:` command that takes a path, an `!` escape, an editor spawn) | the `:` list is closed and static, parsed into typed commands; the only path input is the attachment prompt, handled by the core as a media import; `neo-tui` spawns no process (P3) |
| Event floods during a fast navigator run make the terminal unreadable or slow | 60 ms coalescing tick, ≤ 10 Steer lines/s, virtualised list rendering bounded by the pane height, trace persistence is core-side and independent of the TUI keeping up |
| Ratatui/crossterm upgrades break the rendering goldens on unrelated changes | exact pins in 05 §3; goldens are text dumps with a style legend rather than raw ANSI, so a style-plumbing change fails loudly and a regeneration diff is reviewable |
| Narrow terminals hide the card that needs the user | the tab badge and the status line's queue segment are never dropped; a raised confirm forces the overlay regardless of width, and below 60×20 the TUI still consumes events so nothing is lost |
| "Works in the TUI" is read as "the desktop app is done" | P12 is an ordering rule, not a substitution: the 04 §18 row for the same milestone still has to pass, and the panel/a11y checks have no terminal equivalent |
