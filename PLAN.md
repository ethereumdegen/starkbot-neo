# starkbot-neo — master plan

A voice-first desktop agent for macOS, written in Rust (Tauri v2). You talk (or
type); it drives your browser with **Jev** at classifier speed, designs web
pages, ads and video on an agentic **Hypercanvas**, generates and edits media
through fal.ai and QuiverAI, and strings those together for go-to-market work —
while a UI shows what it hears, thinks, judges and does.

**Start here:** [plans/00-decisions.md](plans/00-decisions.md) is the decision
record. It wins over every other document, including this one.

| Doc | Owns |
|---|---|
| [00-decisions](plans/00-decisions.md) | every firm decision, vocabulary, crate map, milestones, open questions |
| [10-navigator](plans/10-navigator.md) | `jev-nav` — Rust port of `browser-use/jev-ultrafast`: the step loop, TypeSafe wire format, `CdpObserver`, managed Chrome, safety heads, `AxObserver` |
| [03-agent](plans/03-agent.md) | `neo-judge` + `neo-agent` — intake + routing, queue, router, Sol orchestrator on metalcraft, `Gated<T>`, rules, confirms, trace, caps |
| [11-hypercanvas](plans/11-hypercanvas.md) | `neo-canvas` — HTML/CSS frames (Web · Graphic · Set · Video · Board), ops + undo, agent passes, pins, knobs, Jev micro-edits, exports |
| [07-media](plans/07-media.md) | `neo-media` + the `degen-media-maker` library — backends, takes + lineage, tools, quality pipeline, spend gate, enablement |
| [09-gtm](plans/09-gtm.md) | product focus, `extract`, the `neo-gtm` workflows, pacing guard, hard stops |
| [06-packs](plans/06-packs.md) | `neo-packs` — Axoniac pack format, `desktop/` extension, routines, meta-tools, enablement flow, registry |
| [02-voice](plans/02-voice.md) | `neo-voice` — capture, VAD, STT, TTS, duplex, listen states |
| [04-ui](plans/04-ui.md) | app shell, Assist mode, windows + panels, onboarding, settings, Identity, Rust↔TS protocol |
| [01-accessibility](plans/01-accessibility.md) | `neo-ax` — native-app accessibility actor |
| [05-platform](plans/05-platform.md) | workspace, dependencies, SQLite schema, keys, registry, permissions, signing, testing, CI |
| [08-providers](plans/08-providers.md) | direct vendor keys now; provider traits; what StarkRouter must offer later |
| [research/](plans/research/) | archived research that is *not* part of the design |

## 1. How it works

```
 mic ──▶ VAD ──▶ gpt-transcribe ─┐
 chat composer / quick entry ────┴─▶ JEV INTAKE (one request: intent · route · actionable · routine)
                                          │
        amend / answer / cancel ◀─────────┤            chatter → shown greyed, ignored
                                          ▼
                                   QUEUE (SQLite, FIFO) ──▶ ROUTER
             ┌──────────────────────┬──────────────┴───────────┬──────────────────────┐
             ▼                      ▼                          ▼                      ▼
      route: navigate         route: routine            route: design / media     route: multi / question
      JEV NAVIGATOR           fixed steps, Jev-verified  SOL + canvas + media      SOL ORCHESTRATOR
      (zero Sol calls)                                   tools                     navigate() · extract() · ask_user()
             │                                                 │                          │
             └───────────── every acting step: rules → Jev safety heads → confirm card ───┘
                                          │
                                   TRACE ──▶ Mind pane · Steer ticker · Conversation · (optional) speech
```

- **Jev** (TypeSafe; a classifier; ~100–180 ms) is the inner loop. Per step, *one* request answers an `operation` head, a target head per operation over up to 250 observed controls, and the safety heads. No site scripts, no selectors, no profiles.
- A **fast text helper** (`gpt-5.6-luna`, reasoning off) is called only when a field must be typed into.
- **Sol** (`gpt-5.6-sol`) is the orchestrator and the creative: multi-stage work, extraction, answers, long-form copy, art direction, visual critique, recovery from `BLOCKED`. For a plain browser task it is never called.
- The **web** is observed through CDP with one atomic in-page snapshot, in a Chrome profile the app manages. **Native apps** use the same navigator policy through the macOS accessibility API.
- **Design and media** happen on the Hypercanvas, where every frame is real HTML + CSS; the agent builds in layered passes and you refine with pins, knobs, direct edits and ~150 ms Jev micro-edits by voice.
- **Safety** is deterministic rules, then Jev's safety heads in the same request, then a human confirm card. Anything outward-facing, destructive or spending money waits for you.

## 2. Who does what

| | Jev (classifier, constant) | Text helper (fast LLM, on typing) | Sol (reasoning LLM, rare) | Rust (deterministic) |
|---|---|---|---|---|
| Intake | intent, route, actionable, routine match | — | — | control words (stop / yes / no / mute), pre-filters |
| Browser / app steps | operation + target every step; `DONE` / `BLOCKED` | the one value to type | takes over on `BLOCKED` | observe, guards, execute, waits, verification |
| Safety | `outward` · `destructive` · `spends` · `on_task` | — | — | deny lists, secure fields, confirm labels, caps |
| Multi-stage work | — | — | plan as `navigate(goal)` calls, `extract`, `ask_user` | queue, pacing guard |
| Design | micro-edits: target + operation + amount | — | briefs, subtree rewrites, critique | document, ops, undo, tokens, exports |
| Media | — | — | art direction, shoot-out critique via vision | backends, takes ledger, compositor, spend gate |

## 3. Milestones

Defined in [00-decisions](plans/00-decisions.md#milestones-supersede-every-earlier-phase-list); each area doc carries the acceptance criteria for its part. Every milestone is proven in the headless **`neo` CLI** before its UI is built.

```
M0 spikes ─▶ M1 shell ─▶ M2 ears + conversation ─▶ M3 navigator (web) ─▶ M4 judge · queue · safety ─▶ M5 Sol orchestrator
                                                                                  │
                     ┌────────────────────────────────────────────────────────────┤
                     ▼                                                            ▼
              M6 media engine ─▶ M7 hypercanvas I ─▶ M8 hypercanvas II     M9 packs + GTM ─▶ M10 native apps
                                        │
                                        ▼
                                  M12 video            M11 voice out (any time after M4)            M13 ship
```

First usable product = **M0–M5** (talk → it does the browser task → you watch and confirm). First *differentiated* product = **+ M6–M8** (it designs and makes the media too). M9 turns those into GTM workflows.

### M0 spikes — what must be learned before building

| Spike | Question it answers |
|---|---|
| S1 | Rust → CDP: launch the managed Chrome profile, run the snapshot script, make one multi-head TypeSafe request with our key, click the chosen control. Latency of each part. |
| S2 | Can we attach to the user's everyday Chrome (remote-debugging toggle)? If not cleanly, managed profile is the only path. |
| S3 | `gpt-5.6-luna` text-helper latency with reasoning off; Jev accuracy on our own fixture pages. |
| S4 | earshot → `gpt-transcribe`: end-of-speech → transcript time; false-trigger rate in a normal room. |
| S5 | metalcraft + Sol + one tool with reasoning-item replay; do reasoning summaries come through `rig`? |
| S6 | Non-activating NSPanel above a fullscreen app; click-through ring. |
| S7 | Canvas fidelity: a Graphic frame's HTML/CSS → CDP screenshot at 1×/2×/3× vs the same frame in the webview; deterministic frame-stepping of a CSS animation → ffmpeg. |

Numbers and conclusions are recorded in `plans/spikes.md`; any *(verify)* in the docs is resolved there.

## 4. Upstream work in the user's other crates

| Crate | Change | Needed by |
|---|---|---|
| `metalcraft` → 0.12 | reasoning summaries on `ReasoningItem` + `reasoning.summary: "auto"`; image parts in tool results; streaming delta hook; `rig` 0.37 → 0.42 | M5 (thoughts in the Mind pane), M6 (`media_look`), M7 (`canvas_look`) |
| `degen-media-maker` | split into lib + bin; `MediaBackend` trait; progress callback | M6 |
| Axoniac pack format | tolerate the neo-only `desktop/` folder in other hosts | M9 |

## 5. Top risks

| Risk | Control |
|---|---|
| The navigator makes a valid-but-wrong move, or loops | answer validation; freshness + occlusion guards; never retry a mutation; `on_task` head; bounded runs; independent verification in tests; `BLOCKED` → Sol or the user; every decision logged with probabilities for calibration |
| Always-on mic queues junk or acts on a mis-hearing | RMS/duration floor → hallucination filter → Jev intake threshold → "enqueue?" chip for the uncertain band; outward/destructive/spending steps always confirm; everything heard is visible and one click to correct |
| Prompt injection from page or screen text | page text only ever inside delimited, labelled data; rules layer; `on_task` + `outward` heads; confirm cards; packs and `soul.md` can never loosen policy |
| Sites fight automation (bot checks, layout churn) | human-paced actions, per-origin rate caps, no CAPTCHA solving, no account creation; the generic policy needs no per-site maintenance |
| HTML-as-design-source yields messy, unmaintainable frames | strict allowed subset + sanitiser, stable `data-n` ids, token-only values enforced by lint, subtree-scoped rewrites, golden-render tests |
| Media quality is merely "fine" | the quality pipeline (brief → per-model direction → shoot-out → vision critique → targeted edits → our own typesetting), a fixed 20-brief review set before each release |
| macOS permission loss on re-sign | one stable signing identity from M1; `neo doctor` detects "toggle on but untrusted" |
| Spend runaway | per-call / per-task / per-day caps, estimates shown before paid calls, exact usage recorded |

## 6. Open questions for the user

Listed at the end of [00-decisions](plans/00-decisions.md#still-open-needs-the-user). The first one — the Apple signing identity — blocks M1.
