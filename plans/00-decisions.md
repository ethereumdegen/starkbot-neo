# 00 — Decision record (the constitution)

Every other doc must agree with this file. If a doc and this file disagree, this
file wins and the doc is wrong. Dated 2026-09-18. `U` = decided by the user,
`D` = decided in design (changeable, but not silently).

## Product

| # | Decision | By |
|---|---|---|
| P1 | **starkbot-neo**: a macOS (Apple Silicon + Intel) Tauri v2 desktop agent. Bundle id `com.starkbot.neo`. Distributed as a notarized Developer ID DMG with the Tauri updater; never the Mac App Store. | U |
| P2 | **Focus, in order:** (1) browser automation driven by Jev, (2) media creation + editing that must be *very good*, (3) GTM work (= 1 + 2 combined). Native-app automation exists but phases are not ordered around it. | U |
| P3 | **Not a shell/coding agent.** No `bash` tool, no general `read_file`/`write_file`. Terminal-class apps (Terminal, iTerm, Warp, Ghostty, editor terminals) are on the default deny list. File access exists only as media import/export and OS open/save panels. | U |
| P4 | **Voice is the primary input; chat is also there.** One Conversation thread holds heard utterances, typed messages and bot replies. Typed input goes through the same intake → queue → gates path. | U |
| P5 | **Always-on listening is the default** and the UI makes it unmistakable. **Open addressing**: no wake word; any speech while Listen is on is eligible and Jev decides. Push-to-talk and name-required are off-by-default settings. | U |
| P6 | The bot **can talk back** (TTS), optional, **off by default**; when on it speaks **questions only** by default. | U |
| P7 | **Tasks never run while the screen is locked / display asleep.** Queue pauses, a running task stops at the next step boundary as `failed: screen locked`, listening pauses; all resume on unlock. | U (+D details) |
| P8 | **Identity**: default name "Stark", renamable; a user-editable **`soul.md`** (personality, voice, facts about the user, standing preferences). *Preferences, not permissions* — it can never loosen a safety rule. | U |
| P9 | **No website profiles, ever.** Sites are driven generically: observe the page's controls, Jev infers what to do. Native-app profiles are optional technical hints only; everything must work with them removed. | U |
| P10 | **No screenshot/vision fallback for automation in v1**; Screen Recording permission is never requested. (Vision *is* used for judging media and design renders the app produced itself.) | U |
| P11 | **Hypercanvas**: a Figma-like, agent-driven design surface for web pages *and* media (ads, graphics, video), "kind of like the canvas of Claude Design". It absorbs the Studio. | U |

## Models, keys, providers

| # | Decision | By |
|---|---|---|
| K1 | Required keys at onboarding: **OpenAI** and **TypeSafe AI** (Jev). Optional, collected by the media enablement flow: **fal.ai** (`FAL_KEY`) and **QuiverAI** (`QUIVERAI_API_KEY`). Any pack may declare more via `requires_env`. All in the macOS Keychain; never in the webview; never entered by voice. | U |
| K2 | **No OpenRouter.** Later: **StarkRouter**, the user's own gateway fronting OpenAI + fal + Quiver → one key. Until then, direct vendor keys. Provider traits keep the seam (08). | U |
| K3 | Per-use-case model defaults, each user-selectable: inference **`gpt-5.6-sol`** (symbolic `sol-latest` = highest-versioned `gpt-*-sol` from `/v1/models`); fast text helper **`gpt-5.6-luna`**, reasoning off; STT **`gpt-transcribe`**; streaming STT option `gpt-live-transcribe`; TTS **`gpt-4o-mini-tts`**, voice `marin`. Deprecated ids (`whisper-1`, `gpt-4o-transcribe*`, `tts-1*`) are hidden. Prices are read live, never hard-coded. | U (+D) |
| K4 | **No OpenAI image generation.** Images/video/SVG come only from the media backends (fal, Quiver; StarkRouter later). | U |
| K5 | Spend defaults: **$1.00 per task** for ordinary tasks; **design and media tasks get a higher per-task cap (default $5.00)** because page builds and shoot-outs legitimately cost more; **$0.25 per media call before a confirm**; **$10.00 per day** overall. Caps are deterministic arithmetic in Rust (a `SpendGuard`), not a Jev judgment. All configurable in Settings → Safety. | U |

## Architecture

| # | Decision | By |
|---|---|---|
| A1 | **Rust only** behind the webview. **Never Python** — not for spikes, tests, scripts or tooling. JS/TS only where a browser engine requires it (the React UI, in-page `snapshot.js`). Edition 2024, rust-version 1.91. Everything below `src-tauri` is Tauri-free so the whole product runs headless in the **`neo` CLI**. | U |
| A2 | **Jev is a classifier (~100–180 ms/call) and is the fast inner loop**, not just a gate: many Jev calls, few LLM calls. | U |
| A3 | **Navigator = `jev-nav`**, a Rust port of `browser-use/jev-ultrafast` (read as reference, never executed). Whole goal in; per step **one TypeSafe request** with an `operation` head + one target head per operation over ≤ 250 observed controls, plus our safety heads; a fast text helper writes a value **only** on `TYPE_TEXT`. | U |
| A4 | **Web observation = CDP + one atomic in-page DOM/ARIA snapshot**, not the macOS AX tree. Default browser = a **neo-managed Chrome profile** ("Stark's Chrome": launched by the app with a debugging pipe, user signs in to their accounts once, sessions persist). Attaching to the user's everyday Chrome is an opt-in if Chrome's remote-debugging toggle proves workable *(verify in spike)*. The bot works only in tabs it opened. | D |
| A5 | **Native apps use the same navigator policy** through an `AxObserver` over `neo-ax` (objc2 AX bindings, one run-loop actor thread, batched attribute fetch). Safari/Firefox/Electron go this way too. | D |
| A6 | **One TypeSafe wire client** (raw `reqwest`, structured state, object criteria, `model` field) lives in `jev-nav::wire` and is used by every Jev caller. The `jev` crate is not a dependency (its `State`/criteria types are too narrow). | D |
| A7 | **Routing**: Jev intake classifies each utterance/message by *intent* (`new_task · amend · answer · question · cancel · chatter`) and new tasks by *route* (`navigate · design · media · multi · routine:<name>`). `navigate` goes straight to the navigator with **zero Sol calls**; `design`/`media`/`multi`/`question` start Sol. | D |
| A8 | **Sol = orchestrator + creative**, run on `metalcraft` (≥ 0.12) + `rig`: multi-stage tasks as a sequence of `navigate(goal)` calls, `extract`, answering questions, writing long-form copy, art direction and visual critique, and recovery when the navigator returns `BLOCKED`. Fine-grained AX tools behind `Gated<T>` remain as Sol's manual fallback for native apps. | U (+D) |
| A9 | **Safety is layered and cheap**: deterministic rules (deny / must-confirm lists, secure fields, deny-listed apps/origins) → Jev safety heads in the *same* request as the decision (`outward`, `destructive`, `spends`, `on_task`) → human confirm card. Jev outages fail **closed** for risk. Screen/page text is data, never instructions. CAPTCHAs, bot checks, login walls, account creation → `BLOCKED`; the bot never tries to pass them. | D |
| A10 | One **queue**, one **desktop worker** (FIFO; one desktop = one actor). Read-only `question` tasks and **canvas work may run concurrently** with each other, never with a task that is acting on the desktop in the same app. | D |
| A11 | **Packs** = the Axoniac agent-pack format (data only: JSON + Markdown), shared with metalcraft-agent / starfire / degen-tools, plus a neo-only `desktop/` folder (native-app hints, **routines**, tighten-only policies). Built-in capabilities ship as embedded packs. Pack HTTP tools sit behind three fixed meta-tools so Sol's tool list never changes mid-task. Any pack with `requires_env` gets the same generic enablement flow. | U (+D) |
| A12 | **Media engine = `degen-media-maker` as a library** (lib + bin split upstream), wrapped by `neo-media`; backends behind a `MediaBackend` trait (`fal`, `quiver`; `starkrouter` later); the pack ships **in every build, disabled**, enabled by a flow that collects the keys. Takes ledger + lineage kept. Quality pipeline: brief → per-model art direction → shoot-out → Sol vision critique against a rubric → targeted edits → finish → platform export presets. **All text in deliverables is set by our renderer, never trusted to an image model.** | U (+D) |
| A13 | **Hypercanvas document model: every frame is HTML + CSS** (stable `data-n` node ids, CSS custom properties for every token and knob) — web pages, graphics, ad sets, slides and video scenes alike. Rust owns the document (parse → node tree, ops + transactions, per-author undo, tokens, storage); the agent changes frames by **rewriting subtrees** (generative) or by **ops** (micro-edits, knobs, user manipulation). Raster/PDF/video export render through a browser engine (CDP; `WKWebView` snapshot as fallback). `resvg` stays for pure-SVG assets. OpenPencil/Penpot are references, not dependencies. | D (from U's Claude-Design steer) |
| A14 | Canvas refinement UX: **pins** (element-anchored comments → scoped agent tasks), **knobs** (agent-generated sliders bound to CSS variables; dragging costs no model call), direct text edit, and **Jev micro-edits** by voice (`target` + `operation` + `amount` heads over a closed op vocabulary snapped to the token scale; anything generative → `NEEDS_SOL`). | U (+D) |
| A15 | UI: React + TS + Vite in the webview, DRY components + hooks, types generated from Rust. Two modes of the main window — **Assist** (Conversation · Queue · Mind) and **Design** (Hypercanvas) — sharing the always-present listening bar. Overlays (`pill`, target `ring`, quick-entry) are non-activating NSPanels. | U (+D) |
| A16 | Storage: SQLite (`rusqlite`, WAL) for app state; a **studio** is a plain folder (`studio.json`, `takes.jsonl`, `takes/`, `canvas/`, `exports/`) readable by the `dmm` CLI. | D |

## Vocabulary (use these words exactly)

**utterance** (one VAD-cut piece of speech) · **message** (a Conversation entry: voice/typed/bot) · **task** (a queued unit of work) · **route** · **goal** (the sentence the navigator works from) · **step** (one observe→decide→execute cycle) · **head** (one question inside a TypeSafe request) · **operation / target** · **observer** (`CdpObserver`, `AxObserver`) · **text helper** · **gate / confirm card** · **trace** (persisted record of a task) · **Steer ticker** · **pack / skill / routine / persona** · **take** (one generated or imported media result, `t0001…`) · **studio** · **frame** (Web · Graphic · Set · Video · Board) · **node** · **op / transaction** · **token** · **knob** · **pin** · **micro-edit**.

## Crate map

```
neo-core            shared types, events, settings, provider traits, price table
neo-store           rusqlite + migrations
neo-keys            Keychain; the only place a secret string exists
jev-nav             navigator: wire (TypeSafe client), policy, rules, text helper, Observer trait, web/ (CDP), ax/ (feature)
neo-ax              macOS accessibility actor (native apps)
neo-voice           capture, VAD, segmenter, STT, TTS, duplex gate
neo-judge           intake + routing + gates built on jev-nav::wire; verdict log; eval harness
neo-packs           pack bundle/registry, HTTP tool runner, routines, install + lock, enablement
neo-media           adapter over the degen-media-maker lib: tools, backends, spend estimates
neo-canvas          hypercanvas document: HTML/CSS frames, node tree, ops, undo, tokens, knobs, pins, storage
neo-canvas-agent    canvas tools for Sol, outline/look, micro-edit policy, region workers
neo-agent           queue worker, router, Sol orchestrator on metalcraft, tools, Gated<T>, caps, trace
neo-cli             `neo` binary
src-tauri           the app shell
ui/                 React (entries: main, pill, ring, quick-entry)
```

Upstream crates we change: **metalcraft → 0.12** (reasoning summaries, image parts in tool results, streaming deltas, rig bump), **degen-media-maker** (lib + bin split, `MediaBackend` trait).

## Milestones (supersede every earlier phase list)

| M | Name | Core of it |
|---|---|---|
| M0 | Spikes | Rust CDP attach + snapshot + one multi-head Jev request; managed-Chrome launch; earshot → `gpt-transcribe`; Luna text-helper latency; metalcraft + Sol + reasoning replay; NSPanel over fullscreen; canvas frame HTML → CDP screenshot fidelity |
| M1 | Shell | workspace, Tauri app, signing, Keychain, onboarding, permissions, model registry, store, typed bridge, `neo doctor` |
| M2 | Ears + Conversation | capture → VAD → STT, listening bar, tray, Conversation thread + composer + quick entry |
| M3 | Navigator (web) | `jev-nav` core + `CdpObserver` → reference parity; then iframes, shadow roots, upload, contenteditable, new tabs |
| M4 | Judge, queue, safety | intake + routing, queue + worker, rules, safety heads, confirm cards, kill switch, caps, lock pause, trace + Mind pane |
| M5 | Sol orchestrator | metalcraft 0.12, `navigate` / `extract` / `ask_user`, questions, conversation digest, `soul.md` |
| M6 | Media engine | dmm lib split, `neo-media`, backends, enablement flow, quality pipeline, spend gate |
| M7 | Hypercanvas I | document + ops + canvas UI; Graphic / Set / Board frames; takes as nodes; agent passes; pins, knobs, micro-edits; exports |
| M8 | Hypercanvas II | Web frames: breakpoints, components, CDP checks, code export, import-from-URL, handoff bundle |
| M9 | Packs + GTM | installable packs, routines, `neo-gtm` workflows, pacing guard |
| M10 | Native apps | `neo-ax`, `AxObserver`, fine-grained tools + `Gated<T>` |
| M11 | Voice out | TTS, half-duplex gate, headphones full duplex, spoken questions + voice confirms |
| M12 | Video | Video frames, timeline, keyframes, captions, audio, MP4; starflux bridge later |
| M13 | Ship | pill + ring polish, updater, notarized DMG, first-run < 3 min |

## Still open (needs the user)

1. Apple Developer team / signing identity for `com.starkbot.neo` — blocks M1.
2. Personal tool vs public product — sets how much polish M13 needs.
3. Repo home (account, private/public).
4. Default pack registry: axoniac.com or a registry yet to be built.
5. OK to cut metalcraft 0.12, split degen-media-maker into lib + bin, and add a `desktop/` folder to the shared pack format.
