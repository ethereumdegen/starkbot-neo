# starkbot-neo — master plan

A **go-to-market marketing and media harness** for macOS, written in Rust (P2′).
You talk (or type); it drives your browser with **Jev** at classifier speed,
authors and edits media by **operating media apps** — the editors Powermove and
Diffusion Studio and the generator Degen Media Studio — through the same Jev +
accessibility loop, and strings those together for go-to-market work, while a UI
shows what it hears, thinks, judges and does. It is **not** a shell or coding
agent: there is no `bash` tool and no general file access (P3).

It has **two front ends over one core** (P12): a **ratatui TUI** (`neo tui`,
modelled on the OMP coding harness) that is the default surface for development
and milestone acceptance, and the **Tauri v2 desktop app** that ships to users.
Neither holds a secret or makes a decision.

It needs exactly two kinds of credential (K6): a **TypeSafe AI key** for Jev,
plus **one inference connection** — an **OpenAI API key**, a **ChatGPT plan**
through the official Codex app-server, an **Anthropic API key**, or an
**Anthropic (Claude) subscription** through the official Claude Code agent
surface. Speech is OpenAI-API-key only. The media apps own their own fal and
QuiverAI credentials.

**Start here:** [plans/00-decisions.md](plans/00-decisions.md) is the decision
record. It wins over every other document, including this one.

| Doc | Owns |
|---|---|
| [00-decisions](plans/00-decisions.md) | every firm decision, vocabulary, crate map, milestones, open questions |
| [16-quality](plans/16-quality.md) | the **quality upgrade** (Q0–Q5): audit-driven plan to the Pi bar — fail-closed safety, no dead ends (confirm/resume/ask), persistent headed Chrome, M4-lite scope cut, verification spine, feel, Linux support |
| [16-remediation](plans/16-remediation.md) | **what is wrong with the code that exists**, and in what order to fix it: R0 restore CI, R1 safety + truth, R2 user state, R3 navigator contract, R4 resource lifecycle, R5 delete, R6 telemetry |
| [10-navigator](plans/10-navigator.md) | `jev-nav` — Rust port of `browser-use/jev-ultrafast`: the step loop, TypeSafe wire format, `CdpObserver`, managed Chrome, safety heads, `AxObserver` |
| [03-agent](plans/03-agent.md) | `neo-judge` + `neo-agent` — intake + routing, queue, router, Sol orchestrator on metalcraft, `Gated<T>`, rules, confirms, trace, caps |
| [12-media-apps](plans/12-media-apps.md) | **media through apps**: the editors Powermove, Diffusion Studio and degen-paint and the generator Degen Media Studio, driven by Jev + accessibility; the `media-apps` Jev-enablement pack; commanding Powermove's agent; S8 smoke tests |
| [13-degen-media-studio](plans/13-degen-media-studio.md) | **Degen Media Studio** (renamed Degen Media Maker) rewritten in Bend 2, **generation only**: fal + Quiver takes, ledger + lineage, shoot-outs, quotes, send-to-editor, an accessibility-first UI, laws |
| [11-hypercanvas](plans/11-hypercanvas.md) | *(retired 2026-09-19; editing lives in Powermove + Diffusion Studio)* HTML/CSS frames (Web · Graphic · Set · Video · Board), ops + undo, agent passes, pins, knobs, Jev micro-edits, exports |
| [07-media](plans/07-media.md) | *(largely superseded by 12)* takes + lineage, quality pipeline — reference for the quality pipeline |
| [09-gtm](plans/09-gtm.md) | product focus, `extract`, the `neo-gtm` workflows, pacing guard, hard stops |
| [06-packs](plans/06-packs.md) | `neo-packs` — Axoniac pack format, `desktop/` extension, routines, meta-tools, enablement flow, registry |
| [02-voice](plans/02-voice.md) | `neo-voice` — capture, VAD, STT, TTS, duplex, listen states |
| [04-ui](plans/04-ui.md) | the **Tauri desktop** front end: app shell, Assist mode, windows + panels, onboarding, settings, Identity, Rust↔TS protocol |
| [14-tui](plans/14-tui.md) | the **ratatui terminal** front end (`neo tui`, P12): panes, keybindings, the shared event/command seam, TUI acceptance per milestone |
| [17-projects](plans/17-projects.md) | **projects**: a name, a folder, a cadence and two files (`soul.md`, `heartbeat.md`); the per-project heartbeat (default 4 h), tick rows, index + show pages in both front ends |
| [15-heartbeat](plans/15-heartbeat.md) | the heartbeat's semantics (P13, A26, A27): prose format, tick rules, user-declared CLIs — *scoped per project by [17](plans/17-projects.md)* |
| [01-accessibility](plans/01-accessibility.md) | `neo-ax` — native-app accessibility actor |
| [17-linux](plans/17-linux.md) | **Linux** (P16, A36): the AT-SPI `neo-ax` backend, Hyprland window management, Secret Service, XDG paths, `chrome_path()` and the `Ctrl` select-all fix; milestones L0–L4 |
| [05-platform](plans/05-platform.md) | workspace, dependencies, SQLite schema, keys, registry, permissions, signing, testing, CI |
| [08-providers](plans/08-providers.md) | the four K6 inference connections (OpenAI key, ChatGPT/Codex, Anthropic key, Claude subscription); provider + runtime traits; what StarkRouter must offer later |
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
      JEV NAVIGATOR           fixed steps, Jev-verified  SOL + navigate() in      SOL ORCHESTRATOR
      (zero Sol calls)                                   media apps                navigate() · extract() · ask_user()
             │                                                 │                          │
             └───────────── every acting step: rules → Jev safety heads → confirm card ───┘
                                          │
                                   TRACE ──▶ Mind pane · Steer ticker · Conversation · (optional) speech
```

- **Jev** (TypeSafe; a classifier; ~100–180 ms) is the inner loop. Per step, *one* request answers an `operation` head, a target head per operation over up to 250 observed controls, and the safety heads. No site scripts, no selectors, no profiles.
- A **fast text helper** (`gpt-5.6-luna`, reasoning off) is called only when a field must be typed into.
- **Sol** (`gpt-5.6-sol`) is the orchestrator and the creative: multi-stage work, extraction, answers, long-form copy, art direction, visual critique, recovery from `BLOCKED`. For a plain browser task it is never called.
- The **web** is observed through CDP with one atomic in-page snapshot, in a Chrome profile the app manages. **Native apps** use the same navigator policy through the macOS accessibility API.
- **Design and media** happen in media apps that Starkbot operates like any other UI: the editors **Powermove** (AI-native motion and video editor whose own agent can add panels and effects) and **Diffusion Studio** (canvas + timeline), and the generator **Degen Media Studio** (fal + Quiver takes, handed to the editors as files). Sol art-directs and critiques the renders the apps produce; Jev does the clicking; a `media-apps` skill pack supplies vocabulary, hints and routines. Paid actions inside the apps are confirm cards.
- **Safety** is deterministic rules, then Jev's safety heads in the same request, then a human confirm card. Anything outward-facing, destructive or spending money waits for you.

## 2. Who does what

| | Jev (classifier, constant) | Text helper (fast LLM, on typing) | Sol (reasoning LLM, rare) | Rust (deterministic) |
|---|---|---|---|---|
| Intake | intent, route, actionable, routine match | — | — | control words (stop / yes / no / mute), pre-filters |
| Browser / app steps | operation + target every step; `DONE` / `BLOCKED` | the one value to type | takes over on `BLOCKED` | observe, guards, execute, waits, verification |
| Safety | `outward` · `destructive` · `spends` · `on_task` | — | — | deny lists, secure fields, confirm labels, caps |
| Multi-stage work | — | — | plan as `navigate(goal)` calls, `extract`, `ask_user` | queue, pacing guard |
| Design + media (in the apps) | every click in Powermove, Diffusion Studio and DMS | field values, titles, captions | briefs, art direction, shoot-out and render critique via vision, `navigate` goals | confirm gate, grounding probes; the apps own documents, generation, rendering |

### 2.1 Product user stories

These are end-to-end product contracts, not demo prompts. The default path uses deterministic Rust plus Jev's classifier-speed decisions; Sol is reserved for planning, copy, visual direction, critique, and recovery.

#### SEO: audit and improve a local or remote website

> As a site owner, I can point Stark at a local website project or a remote URL and ask it to find, prioritize, and fix SEO problems, so I get measurable improvements without paying for an LLM call on every page or field.

- **Remote URL:** Stark opens the site in managed Chrome, crawls within an explicit origin/page cap, extracts rendered metadata, headings, links, canonicals, robots directives, structured data, performance signals, and accessibility semantics deterministically, and uses Jev for cheap navigation and page-state decisions. It may edit through an authenticated CMS only when the user asks; every publish or outward-facing change requires confirmation. Otherwise it produces an audit and concrete patches.
- **Local project:** the user grants one project folder through the macOS open panel. A scoped website-project capability may read and patch only that folder—never arbitrary files or a shell. Rust handles crawling, duplicate detection, link graphs, schema validation, and before/after checks; Sol is used once for prioritization and only for genuinely generative title, description, or page-copy rewrites. Every change is previewed as a diff and can be reverted.
- **Acceptance:** report issues by impact and affected URL; distinguish source HTML from rendered output; preserve framework/build conventions; re-run the same audit after edits; show score deltas and unresolved items; never claim ranking gains; no paid model call for deterministic checks.

#### Browser use: fast Jev navigation over accessibility semantics

> As a user, I can give Stark a browser goal in ordinary language and watch it complete the task quickly and safely, including forms, menus, iframes, uploads, scrolling, and new tabs.

- Managed Chrome uses the atomic CDP DOM/ARIA snapshot: browser accessibility semantics without the latency and instability of walking Chrome through the macOS AX API. Jev receives one bounded action space and chooses operation, target, progress, and safety heads in one request per step; the Luna helper is used only for text values; ordinary browser tasks make zero Sol calls.
- Safari, Firefox, and opaque Electron browser shells use the macOS AX path as a secondary route. The same `jev-nav` policy and safety gates apply to both observers.
- **Acceptance:** median observe → Jev → act stays interactive; targets are independently verified after mutations; open shadow roots, cross-origin iframes, contenteditable, file inputs, nested scrolling, popups, and persistent sessions work; stale or occluded targets are never clicked; login walls and CAPTCHAs return `BLOCKED`.

#### macOS use: generic native-app control through Accessibility

> As a Mac user, I can ask Stark to operate ordinary native and Electron apps without app-specific scripts, while secure fields and dangerous actions remain protected.

- `neo-ax` snapshots the focused app through batched Accessibility API reads; Jev drives the same operation/target loop used for the browser. AX actions are preferred, with guarded `CGEvent` fallback only when the target is fresh, visible, unobscured, and inside the expected window.
- Optional pack hints may enable AX trees or tune waits, but cannot encode selectors or workflows. Sol receives fine-grained gated AX tools only after the cheap navigator reports `BLOCKED`.
- **Acceptance:** works with hints disabled across the fixture app and representative AppKit, SwiftUI, Catalyst, and Electron apps; secure-field values never enter memory, logs, or model state; stale references relocate only on one exact fingerprint match; focus, modifiers, Spaces, screen lock, and permission loss fail safely.

#### Media through apps: Powermove, Diffusion Studio and Degen Media Studio

> As a marketer or creator, I can brief Stark and watch it make media in real apps. It generates stills, motion and vectors in Degen Media Studio, then cuts and animates them in Powermove or Diffusion Studio. I keep full, editable projects, and Stark only ever holds my OpenAI and Jev keys.

- Starkbot operates the apps' UIs with the Jev navigator: `AxObserver` for the macOS apps (Diffusion Studio first; Powermove is Electron, with `AXManualAccessibility`), and the CDP snapshot for web UIs (DMS, `powermove serve`). The `media-apps` Jev-enablement skill pack adds app vocabulary, technical hints, Jev-verified routines (import, export, shoot-out, send to editor) and Sol goal templates. It never uses per-site selectors and collects no keys ([12](plans/12-media-apps.md)).
- Sol owns briefs, art direction, the quality pipeline and vision critique of renders the apps produced. Jev does every click. Generate, render-with-credits, export-overwrite, publish and **requests to Powermove's self-rewriting agent** are confirm cards.
- Degen Media Studio (renamed Degen Media Maker) is rewritten in Bend 2 and **thinned to generation**: fal + Quiver takes with lineage, shoot-outs, contact sheets, quotes, and send-to-editor with sidecars. It has proven ledger and spend laws and an accessibility-first UI ([13](plans/13-degen-media-studio.md)). The Hypercanvas is retired.
- **Acceptance (smoke test S8):** S8a produces a 10 s 9:16 promo in the Diffusion Studio macOS app (ffprobe + filmstrip checks, 4 of 5 runs). S8b makes the same promo in Powermove, plus a panel created by Powermove's agent and used by Jev. S8c goes DMS shoot-out → star → animate → send to editor → a teaser cut in Diffusion Studio. All run with no fal or Quiver key anywhere in Starkbot.

## 3. Milestones

The milestone list lives in one place: the table in
[00-decisions](plans/00-decisions.md#milestones-supersede-every-earlier-phase-list),
which runs M0–M14 and wins over any sequence restated elsewhere, including the
picture below. Each area doc carries the acceptance criteria for its part.
Every milestone is proven headlessly in the **`neo` CLI** and then in the
**`neo tui` terminal front end**; the desktop UI follows (P12,
[14](plans/14-tui.md)).

The picture is dependency order, not the numbering — M4′ was inserted after
M4, and M7/M8 are retired (A13′: editing is Powermove + Diffusion Studio):

```
M0 spikes ─▶ M1 shell ─▶ M2 ears + conversation ─▶ M3 navigator (web) ─▶ M4 judge · queue · safety ─▶ M5 Sol orchestrator
                                                                             │                               │
      M4′ heartbeat — a tick is an ordinary                  ◀───────────────┤                               ▼
      gated task, so it needs M4's                                           │                 M6′ media via apps (S8)
      queue, caps and confirm cards                                          │                 (M7/M8 retired; Degen Media
                                                                             │                  Studio is Bend G0–G4, own repo)
      M12 voice out — any time after M4                      ◀───────────────┘                               │
          ┌──────────────────────────────────────────────────────────────────────────────────────────────────┘
          ▼
      M9 packs + GTM ─▶ M10 component SDK ─▶ M11 native apps ─▶ M13 video ─▶ M14 ship
```

M11 owns `neo-ax` and the `AxObserver`, but S8a needs that observer to drive
the Diffusion Studio macOS app, which is why the spike table below pulls a
`neo-ax` spike forward ahead of M6′.

First usable product = **M0–M5** (talk → it does the browser task → you watch and confirm), usable from `neo tui` before the desktop shell is finished. First *differentiated* product = **+ M6′** (it makes the media too, by operating Powermove, Diffusion Studio and Degen Media Studio). M9 turns those into GTM workflows.

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
| S8 | Media through apps smoke tests ([12 §5](plans/12-media-apps.md#5-smoke-tests-spike-s8-before-m6)): S8a Diffusion Studio **macOS app** via `AxObserver` (needs a `neo-ax` spike ahead of M6′; M11 owns the finished observer), a 10 s 9:16 promo; S8b the same in Powermove plus a panel made by its agent; S8c DMS generate → send to editor → cut. Only OpenAI + TypeSafe keys. |

Numbers and conclusions are recorded in `plans/spikes.md`; any *(verify)* in the docs is resolved there.

## 4. Upstream work in the user's other crates

| Crate | Change | Needed by |
|---|---|---|
| `metalcraft` **1.0.1** (published) | shipped: `rig` 0.42, reasoning-item replay, summaries, deep-merged request params, an append-only `Journal` with rewind/fork, cancellation through `NodeCtx`, and a `Telemetry` event stream correlated to journal entries (usable for the Mind pane and cost views). Still open: image tool-result parts, the streaming delta hook, and the **rig Anthropic Messages provider for the K6 Anthropic-API-key runtime** — `ReactAgentNode` only extracts OpenAI-shaped reasoning (`Summary`/`Encrypted`) and drops Anthropic's `ReasoningContent::Text { text, signature }`, so a thinking-plus-tools turn cannot be replayed | M5 (reasoning spine, A22 runtimes), M6 (`media_look`) |
| `degen-media-maker` | renamed **Degen Media Studio**; rewritten in Bend 2 in `~/ai/degen-media-studio-bend`, generation only, accessibility-first UI ([13](plans/13-degen-media-studio.md)) | M6′ (S8c) |
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
| Media apps change their UI, or expose timelines only on `<canvas>` | no selectors (P9); Jev-verified routines fall back to plain navigation; S8a/S8b check Diffusion Studio's and Powermove's accessibility first (Powermove also offers `powermove serve` → CDP); DMS is accessibility-first by requirement; upstream issues/PRs (both editors are open source) |

## 6. Open questions for the user

Listed at the end of [00-decisions](plans/00-decisions.md#still-open-needs-the-user). The first one — the Apple signing identity — blocks M1.
