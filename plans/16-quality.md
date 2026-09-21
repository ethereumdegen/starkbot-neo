# 16 — The quality upgrade: as good as Pi

**Bar:** Starkbot Neo should feel the way Pi and Hermes feel — every interaction
fast, honest, and recoverable; nothing dead-ends; nothing lies about its own
state; the product is *smaller* than its plans and *better* than its demos.
Not more complex. As good.

This document is an audit-driven upgrade plan. Every claim below was verified
against the working tree on 2026-09-21 (file:line citations throughout). It
proposes work in six phases, Q0–Q5, each provable in `neo tui` per P12.
Where it contradicts an area doc, it lists the amendment for
[00-decisions](00-decisions.md) at the end; until ratified, 00-decisions wins.

---

## 0. What "good" means, measurably

Pi's quality is not features; it is the absence of bad seconds. The bar,
as tests or release gates:

| # | Bar | Today | Gate |
|---|---|---|---|
| B1 | **Never acts unsafely on malformed input.** A missing/invalid safety answer pauses the run; it never executes. | Missing head scores 0.0 and the click executes (`jev-nav/src/lib.rs:170`) | Unit test: decision with absent `outward` head → run pauses, nothing acted |
| B2 | **No dead ends.** Every way a run stops carries a next step the user can take from the same screen: confirm it, sign in, supply a value, or drop it. | Confirm/sign-in/unknown-value/off-task all end the run with a prose string (`lib.rs:177`, no `NeedsUser`) | Enumerate `Outcome`; every non-`Done` variant renders as an actionable card in the TUI goldens |
| B3 | **Interactive latency.** No TUI frame-loop stall > 50 ms; a hung Jev call costs ≤ 5 s, not 25 s; Chrome is warm when the user is mid-conversation. | `block_on` for key-check/model-refresh/disconnect inside the frame loop (`neo-tui/src/run.rs` ~866, ~1290); wire timeout 25 s (`jev-nav/src/wire.rs:113`); Chrome cold-launched per run (`neo-agent/src/agent/tools.rs:292`) | Frame-loop audit test; wire timeout constant; keep-warm launch measured in the nav summary line |
| B4 | **Logged-in reality.** The default browser run can complete a goal behind a login the user performed once. | Default = headless + throwaway temp profile per run (`neo-cdp/src/lib.rs:60`, `tools.rs:292-303`) — anything behind a login is unreachable | Review-set task "reply to the latest message in <webmail fixture>" passes on the persistent profile |
| B5 | **Typing always works.** A goal that requires typing never dies mid-run for lack of a helper. | Fresh install has no text helper (`neo-agent/src/nav.rs:134` test: "the default runtime cannot type yet"); runs launch with a warning and stop at the first `TYPE_TEXT` (`tools.rs:53-54`) | Nav refuses to *start* without a typing path, with a one-keystroke fix; helper answers < 1.5 s p50 |
| B6 | **The docs tell the truth.** README, PLAN.md and the code agree on what exists. | README sells the retired Hypercanvas; "Milestone 1" prose beside M3/M5 capabilities; PLAN.md ends at M13, 00-decisions at M14 | Drift review is a release-gate checklist item |
| B7 | **A fixed review set gates releases.** Like the plan's 20-brief media set, but for what exists now: navigation. | neo-eval is real but has no canonical corpus; jev-nav's loop is CI-tested only against `FakeObserver` | 15-task nav review set, 4-of-5 consensus, run before each tag |
| B8 | **Runs on Linux.** The core product — TUI, providers, Jev browser navigation, STT — works on a stock Ubuntu/Fedora box; macOS-only surfaces refuse honestly. | Workspace is macOS-only: `neo-ax` (objc2/AX), Apple keychain in `neo-keys`, Apple STT + TCC in `neo-voice`, `macos-private-api` in src-tauri; CI builds only macos-14 | Ubuntu CI lane green; the browser review set passes on Linux |

Everything below serves one of B1–B8.

---

## 1. Ground truth (what the audit found)

### 1.1 Genuinely good — protect it

- **neo-agent** is the real product: metalcraft ReAct loop with streaming,
  steering at node boundaries, ordered cards, cancellation that keeps partial
  answers; 4 provider paths with header-discipline tests; PKCE OAuth with an
  RFC 7636 vector test (~137 tests).
- **neo-ax** is deep: guard pipeline (generation, relocate-once, frontmost,
  occlusion hit-test), secure-field redaction tested at three layers, deny
  list below every entry point, kill switch honored mid-typing.
- **CdpObserver + snapshot.js** handle OOPIFs, shadow roots, contenteditable
  read-back, uploads, popups, nested scroll — with a live parity spike proving
  it (`spikes/s1-nav/src/bin/parity.rs`).
- **neo-store** actor model, migration discipline (v1 seed fixture, backups,
  newer-schema refusal), atomic settings merge.
- **neo-tui** pure reducer/keymap/renderer seams, 16 insta goldens, honest
  "not built yet" refusals, kill switch from every mode.
- **~380 behavioral tests** with rationale comments; CI runs fmt, clippy -D,
  workspace tests, cargo-deny, and bespoke architecture greps on macos-14.

The upgrade must not flatten any of this. It is why the plan is tractable.

### 1.2 Paper — designed, schema'd, unbuilt

- The whole 03-pipeline: intake → queue → router → executors, `Gated<T>`,
  confirm broker, `VerdictSink`, SpendMeter, trace persistence. Twelve tables
  from `0001_init.sql` (tasks, confirms, jev_verdicts, trace_items, …) have
  **zero Rust readers or writers**.
- neo-judge is a 47-line key validator wearing an "intake, routing and gates"
  Cargo description.
- Plan-04's product shell (panels, listening bar, tray, onboarding), plan-02's
  always-on voice. Both front ends currently drop queue/confirm events on the
  floor (TUI: activity ring only, `state.rs:3572`; webview: default case).

### 1.3 Broken or contradicting its own contract

| Finding | Evidence | Severity |
|---|---|---|
| Safety heads **fail open** on missing answers | `decision.safety.get(head).copied().unwrap_or(0.0)` — `jev-nav/src/lib.rs:170` | **P0** |
| Yes/no head wire shape (`"noul"`) unverified; if wrong, every head silently returns `None` → unguarded runs, no error | `wire.rs:66-72`, `policy.rs:222`; plan flagged *(verify)*, never resolved | **P0** |
| Deterministic rules layer absent: no confirm-word list, denied origins, captcha/password/payment signals, payment-field blanking | `rules.rs` has head questions only; `snapshot.js` signals only `cross_origin_frames` | **P0** |
| `on_task` head asked, collected, never enforced — the injection tripwire doesn't exist | `lib.rs:169` checks only outward/destructive/spends | P1 |
| Every human-in-the-loop path is a dead end: risky → `Blocked(String)`, run over, Chrome closed | `lib.rs:175-181`, `tools.rs:826-830` | **P0** (product) |
| Default Chrome is headless + throwaway per run — the plan's persistent headed "Stark's Chrome" inverted | `neo-cdp/src/lib.rs:60`, `tools.rs:292-303` | P1 |
| AX SELECT can never resolve an option; AX PRESS_KEY hardcodes Return and is unreachable | `jev-nav/src/ax.rs:252-268`, `:109-116` | P1 |
| `fingerprint()` serializes rects → any animated pixel defeats the no-progress tripwire | `lib.rs:249-257` | P2 |
| `AXEnhancedUserInterface` set on every observation; plan 01's first consequence is that it is **never** set | `neo-ax/src/actor.rs:560-583` vs 01 | P2 (ratify or revert) |
| TUI frame loop blocks on vendor HTTP (key check, model refresh, disconnect) | `run.rs` ~866, ~1290 | P1 |
| neo-cdp: **zero tests**; parity evidence is a manual spike | `crates/neo-cdp/` | P1 |
| UI never built/tested in CI (vitest + tsc exist, unwired) | `.github/workflows/ci.yml` | P2 |
| Live-looking TypeSafe key in plaintext `.env` (untracked, but on disk and dotenv-loaded by spikes) | `.env` | P1 (rotate) |
| eval `ACTIONS` lists `ask` (no such tool) and omits `ax` (registered) — misdescribes the surface to the judge | `neo-eval/src/lib.rs:110` | P2 |
| README/PLAN drift: Hypercanvas, milestone table, "Milestone 1" | README:3-5, PLAN.md vs 00-decisions | P2 |
| `metal.rs` 2,666-line monolith — exactly where Q2's work must land | `agent/metal.rs` | P2 (pre-factor) |

---

## 2. Principle: subtract before adding

"As good as Pi, not necessarily as complex." The single biggest risk to
quality here is the 400 KB plan corpus pulling effort into breadth. This plan
proposes the following **scope cuts** (amendments in §8):

- **C1 — M4 shrinks to M4-lite.** No FIFO queue, no intake heads, no router,
  no lanes. One conversation drives **one live task** with a full lifecycle:
  `running → needs_confirm | needs_user → resumed | dropped`. The 12 dormant
  tables stay dormant except `confirms` and `trace_items` (rebuilt to fit,
  see Q2). The queue returns only if real usage demands parallel tasks —
  Pi never needed one.
- **C2 — Voice stays push-to-talk** through this plan. Plan-02's always-on
  pipeline (VAD, segmenter, listen states, duplex) is deferred; today's
  capture + STT is already honest and tested. One addition only: STT
  keyword hints (bot name, app names) — small, direct accuracy win.
- **C3 — The desktop shell stays a developer harness.** All product-surface
  polish lands in the TUI first (P12 already says this). No panels, tray, or
  onboarding work in Q0–Q4. The webview only gains what parity requires:
  rendering the same confirm/ask cards from the same events.
- **C4 — No new operations** (EXTRACT, web MENU), no packs, no heartbeat, no
  GTM workflows until B1–B7 hold. A small excellent loop beats a wide
  mediocre one.

What is explicitly **not** cut: safety, escalation, the persistent browser,
AX correctness, verification. Those are the product.

---

## 2.1 Execution status — 2026-09-21

Q0–Q3 and Q5's L0/L1 seams are **built, tested and verified live**; Q4 (feel)
is the remainder. What landed, against the bars:

| Bar | State | Proof |
|---|---|---|
| B1 fail closed | done | `wire::Evaluation::noul` errors on an absent or mis-shaped head; `policy::resolve` requires every asked head; the loop scores an unknown head `1.0`. The live `noul` shape is now recorded in [spikes.md](spikes.md). 4 tests, incl. two that execute nothing on a broken response |
| B2 no dead ends | done | `Outcome::{Done, NeedsConfirm, NeedsUser, Blocked{BlockReason}}` with `Escalation` + `ResumeToken`; `Approval::{Approve, Deny, Ready, Value}`; deterministic `gate.rs`; broker in `neo-agent/src/confirm.rs`; cards render in TUI and webview and are answerable from the terminal. **Live:** a real Chrome run on `nav-pay.html` paused on “Pay”, `y` resumed it to `Done` with exactly one click; piped stdin refused it and the run ended honestly |
| B3 latency | done | wire timeout 5 s; no network work on the TUI frame loop (`Chores` + a 50 ms `debug_assert` that was falsified against the old code); `doctor` caches its three macOS probes — a keyless `neo doctor` measured 90 ms, was 4.5–5 s |
| B4 logged-in reality | done | headed persistent managed profile by default, attach-to-running, tab left open and activated, 8-tab ledger; `--headless` is the eval/CI path. Live-attach test against a real Chrome |
| B5 typing | done | a run refuses to start without a text helper, naming the fix; the helper resolves a fast (Luna) tier model |
| B6 docs | done | README and PLAN.md re-grounded; the CI architecture rule now states an invariant that can actually pass |
| B7 review set | done | 15 `nav-review` cases + 3 `nav-review-live`, 18 self-contained fixtures, `RELEASING.md` as the gate |
| B8 Linux | L0/L1 seams done | Secret Service backend behind a platform seam, neo-voice/neo-ax/Tauri deps target-gated, required Ubuntu CI lane, honest target list. **Not yet proven on real hardware** — see below |

Repo-wide: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
-D warnings`, `cargo test --workspace` (518 tests) and `cargo deny check` are
all green — the first three of those were red on this tree before this pass.

Known gaps, recorded rather than papered over:

- **Linux is unproven on Linux.** Every seam is cut and cross-checked where a
  cross-toolchain allowed it, but no build has run on a Linux machine; the
  Ubuntu lane's first real run is the test. `neo-desktop` and the two Tauri
  spikes are excluded from that lane (L2 deferred).
- **`nav-upload-confirm` is tagged `known-gap`.** An agent turn has no way to
  offer an attachment (`browse` passes `attach: []` and the observer drops
  upload actions without one), so the first-upload confirm cannot trip from
  an agent turn — only from `neo nav --attach`. The release bar is 14 of 15
  plus the tagged gap until that seam exists.
- **The `.env` TypeSafe key still needs rotating by hand.** CI now refuses a
  tracked `.env`, but the key on this machine was live during development.
- **Q4 (feel) is not started**: the first-run walkthrough, the language pass
  over every user-facing string, and ratifying A-Q5/A-Q6.

---

## 3. Q0 — Stop the bleeding (days, not weeks)

Small diffs, immediate honesty. No architecture.

1. **Fail closed** (B1): missing/invalid safety answer ⇒ treat as
   `1.0` (risky) and surface `head unanswered` in the step event; add the
   malformed-wire unit test. One-line semantics change at `lib.rs:170` plus
   validation in `wire.rs` that all four heads came back well-formed.
2. **Resolve the `"noul"` *(verify)*** against the live TypeSafe API once,
   record the answer in `plans/spikes.md`, and make a mismatched head shape a
   hard `NavError::Wire` — never a silent `None`.
3. **Wire timeout 25 s → 5 s** (the plan's own deliberate number), retries
   unchanged (`wire.rs:113`).
4. **Rotate the `.env` TypeSafe key**; move spike loading to the keychain
   flow (`neo keys set typesafe`); delete `.env`. Add a repo check that no
   `*.env` exists in the tree.
5. **Docs truth pass** (B6): README drops the Hypercanvas sentence and the
   plans/11 link from the opening; PLAN.md cites 00-decisions' M14 table
   instead of restating M13; status paragraph states the observed truth
   (M0 evidenced, M1 done except signing, M3 + a slice of M5 runnable,
   M4 unstarted).
6. **Eval drift**: `ACTIONS` = [browse, app, ax, answer, probe, fixture]
   (`neo-eval/src/lib.rs:110`).
7. **CI fast wins**: `Swatinem/rust-cache`, a concurrency group to kill the
   push+PR double-run, and a Node job running `tsc --noEmit` + `vitest run`
   for `ui/` (B6's cheapest guard). A `justfile` naming the five commands a
   contributor needs.

**Acceptance Q0:** the malformed-safety test exists and fails on the old
code; a `git grep -i hypercanvas README.md` is empty; CI runs the UI checks;
`.env` gone and key rotated.

---

## 4. Q1 — The browser that feels right (B3, B4, B5)

The navigator is the product's hands. Three changes make it feel like Pi
instead of a batch job:

1. **Stark's Chrome, for real** (plan 10 §10, currently inverted):
   - Persistent profile at `~/Library/Application Support/starkbot-neo/chrome`,
     **headed by default**, `headless` only behind `--headless`/setting.
   - Lazy-launch, keep-warm: first browser mention in a conversation may
     pre-launch; runs attach to the running instance; `Browser.close` on quit.
   - Owned-tab cap (8); on `Done`, the tab is **left open and activated** —
     the user sees what was done. On confirm-pause, the tab stays exactly
     where it is (prerequisite for Q2 resume).
   - Sign-in flow: user signs into sites once in this Chrome, headed. This is
     what makes B4 pass.
2. **Typing guaranteed** (B5): `run_browser`/`run_app` refuse to *start*
   without a text helper, naming the one-keystroke fix (pick a runtime),
   instead of launching with a warning and dying mid-run
   (`tools.rs:275-277`). Honor `settings.models.text_helper` with a
   fast-tier default (Luna-class, reasoning off) rather than riding the
   Sol-class model per fill; record `text_ms` p50 in the run summary.
3. **Latency discipline** (B3):
   - TUI: replace the frame-loop `block_on`s with Jobs (the login path
     already shows the pattern — `run.rs LoginWorker`); add a debug assertion
     that `execute` never blocks > 50 ms.
   - Keep the parity spike's measured budgets visible: the nav summary line
     already prints median Jev latency and protocol calls; add observe and
     act medians so regressions are seen, not felt.
4. **Fix the AX limbs** (the native path silently degrades to click/fill):
   - SELECT: `actions_of` emits per-option actions (or an `option` field the
     resolver actually reads) — today `ax.rs:252-263` can never resolve one.
   - PRESS_KEY: either offer real key actions from the AX table or remove the
     operation from the AX action space; a hardcoded-Return dead path is
     worse than absence.
   - Marker: fold focused-element value into `marker_of` (`ax.rs:137-145`) so
     a changed field invalidates full freshness, matching the web path.
5. **Fingerprint without geometry** (`lib.rs:249-257`): canonical semantics
   only (roles, labels, values, structure) so the no-progress tripwire
   survives animated pages.

**Acceptance Q1:** B4's logged-in review-set task passes; a nav run against
the fixture corpus shows warm-start attach < 300 ms; AX select works in the
fixture app; `cargo test -p jev-nav` covers the geometry-free fingerprint;
starting a typing goal with no helper is a refusal with a fix, not a run.

---

## 5. Q2 — No dead ends (B2) — the heart of the upgrade

This is M4-lite (cut C1). Everything that today ends a run becomes a
conversation the user can finish. The TUI's entire CARD apparatus (Mode::Card,
`Card{armed,rendered}`, debounce, keymap tests) already exists and is dead
(`state.rs:801` — "M4 fills this"); this phase brings it to life.

### 5.1 Outcome taxonomy in jev-nav

Replace `Outcome::Done | Blocked(String)` (`lib.rs:37-41`) with the plan's
shape, minimally:

```rust
enum Outcome {
    Done { tab: Option<TabHandle> },
    NeedsConfirm { escalation: Escalation, resume: ResumeToken },
    NeedsUser { reason: NeedsUser /* SignIn | Value | Captcha */, resume: ResumeToken },
    Blocked { reason: BlockReason },   // DeniedOrigin, NoProgress, BudgetExhausted, OffTask…
}
```

`Escalation` carries the decision, the observed element, recent history and
the safety probabilities — enough for a card and for Sol, instead of prose.
`ResumeToken` holds what re-entry needs (page key / AX generation, pending
decision, approval scope). A `wait`-free pause: the browser stays open
(Q1.1), the AX generation is re-guarded on resume, and a stale resume simply
re-observes — the loop already knows how (`lib.rs:210-221`).

### 5.2 Deterministic rules layer (plan 10 §7, absent today)

Before any Jev decision executes: denied-origins check, non-http(s)
navigation guard, the confirm-label word list (`send | post | delete | pay |
subscribe | …`), payment-field exclusion + blanking, and `password_field` /
`captcha_frame` page signals in `snapshot.js` (today: only
`cross_origin_frames`). Captcha ⇒ `NeedsUser{Captcha}`; login wall ⇒
`NeedsUser{SignIn}` — sign in, press resume, the run continues. Enforce
`on_task` with the plan's strike logic ⇒ `Blocked{OffTask}`. Upload gets its
first-use-per-origin confirm (plan 10 §11.3).

### 5.3 The confirm broker in neo-agent

A tripped head or rule pauses the run and publishes
`AppEvent::ConfirmRequest` with the `Escalation`. Approval resumes via the
`ResumeToken`; denial converts to a steer ("don't do X; try…") so the turn
continues intelligently instead of dying. Approvals are single-shot in Q2;
`remembered_allows` comes only after the review set shows repeated identical
confirms (subtract first). Persist the decision trail to `confirms` +
`trace_items` — **rebuild those two tables to fit the code** (migration, per
the messages-v4 precedent); the other ten dormant tables get dropped by the
same migration and return only when something reads them.

### 5.4 The `ask` tool

Register `ask` in the agent graph (`metal.rs:1189-1206` registers only
browse/app/ax) so Sol can ask one question mid-turn; it rides the same card
path (`AskRequest` → CARD mode → answer becomes a steer). `ActionKind::Ask`
finally gets constructed in production. Text-helper `{"text": null}` maps to
`NeedsUser{Value}` through the same seam (today it's a run-fatal
`TextError::Invalid`, `text.rs:137-143`).

### 5.5 Both surfaces render it

TUI: populate `state.card` from `ConfirmRequest`/`AskRequest`; the y/n
armed+rendered rule and its tests already exist. Webview: model the same two
variants in `api.ts` + `reduce.ts`. **Create the shared fixture corpus**
(plan 14 §8's drift mitigation, missing today): one JSONL of envelopes driving
both the insta goldens and the vitest reducer tests, so the two front ends
cannot drift silently.

### 5.6 Pre-factor

Before any of this lands: split `metal.rs` (2,666 lines) into
`loop / reporter / cards / providers-glue` modules, tests moved beside their
subjects. Mechanical, no behavior change, reviewed alone.

**Acceptance Q2:** the review set includes one task that trips `spends` —
the run pauses with a card, approval completes the purchase-free variant,
denial steers; one login-wall task resumes after a human signs in; goldens
show the confirm card at all four widths; `Outcome` has no `String` reasons
left; eval judge sees `ask` used.

---

## 6. Q3 — Verification spine (B7)

Quality regressions must be caught by machinery, not vibes.

1. **neo-cdp tests** (today: zero). Unit-test the transport framing (pipe
   NUL-frames, session routing) against a scripted fake; promote the parity
   spike to `cargo test -p jev-nav --test parity -- --ignored`-style gated
   live tests, and give CI a weekly scheduled job that runs them plus the
   live-ignored AX tests on a self-hosted/nightly lane. A parity regression
   currently ships invisibly.
2. **The nav review set** (B7): 15 fixed tasks over the existing fixture
   pages + 3 real sites — forms, iframes, shadow DOM, upload, popup, login
   wall (resume), confirm trip, AX fixture app select/menu. Run through
   neo-eval's consensus machinery before each tag; the report is the release
   note's first section.
3. **Judge the judge**: neo-judge either becomes the (M4-lite-sized) home of
   the rules layer + verdict logging from Q2, or its Cargo description
   shrinks to the truth. No crate may describe itself as something it isn't.
4. **jev verdict logging**: persist per-step head probabilities
   (`jev_verdicts`, resurrected with the Q2 migration) so confirm thresholds
   get calibrated from data — the plan promised calibration; nothing logs
   today.

**Acceptance Q3:** CI is green with the new lanes; the weekly live lane has
run at least twice; a release checklist exists and cites the review-set
report.

---

## 7. Q4 — Feel (the Pi test)

The last mile is disproportionately what users call "wonderful":

1. **First run**: `neo tui` on a fresh machine reaches a working first
   conversation in under two minutes — Connections page first (already true),
   each unmet doctor row names its one command, and the moment the last
   prerequisite lands the composer invites a first goal. Measure it with the
   fresh-install golden plus a scripted walkthrough.
2. **Progress you can trust**: while a nav run is live, the TUI's run pane
   shows the step stream (already flowing via `StepEvent`) with the label of
   what it is about to do *before* it does it — the confirm rule generalized:
   never surprise. Headed Chrome (Q1) makes the same thing visible in the
   real window.
3. **Language pass**: every user-facing refusal, warning, and card follows
   one voice — short, concrete, next-step-first ("Sign in to continue — I'll
   pick up where I stopped." not "Blocked: login wall"). Audit the ~30
   status-line strings in `state.rs` and the card texts in one sitting.
4. **Sound of silence**: no stray output — already enforced
   (`print_stdout = deny`); extend the audit to tracing noise at default
   level so `neo ask` output is exactly the answer.
5. **Ratify or revert the two contract deviations** (§8): AXEnhancedUserInterface
   and Apple-STT-as-default. Either the plan changes or the code does;
   disagreement is the one state that may not persist.

**Acceptance Q4:** a recorded fresh-machine walkthrough hits the 2-minute
bar; the review set passes 4-of-5; the language audit is a single PR the
user reads top-to-bottom.

---

## 8. Q5 — Linux (B8)

Portability is a seam problem, not a rewrite: the product's spine — the
ReAct loop, providers/OAuth, jev-nav, the CDP client (`command-fds` pipe
transport is plain Unix, Chrome's `--remote-debugging-pipe` flags are
identical), neo-store, neo-otel, the crossterm TUI — is already
platform-neutral Rust. What binds the workspace to macOS is five specific
attachments. Q5 cuts the seams, ships the browser product on Linux, and
defers the genuinely hard part (native-app control) behind demand, exactly
like the queue (C1).

### 8.1 Tiers

| Tier | Scope | Verdict |
|---|---|---|
| **L0** | The workspace compiles and its portable tests pass on Linux; macOS-only code is `cfg`-gated, not stubbed | Do in Q5 |
| **L1** | The product core: `neo tui` + doctor + all K6 connections + Jev **browser** navigation + push-to-talk STT + eval on Linux | Do in Q5 — this is the Pi-grade product; the browser was always the main act |
| **L2** | Desktop shell via Tauri/webkit2gtk | Defer — C3 already keeps the shell a dev harness on macOS; Linux inherits that priority |
| **L3** | Native-app control (`neo app`/`neo ax`) via AT-SPI2 | Defer behind demand — see 8.4 |

### 8.2 The five seams (L0)

1. **neo-keys**: the backend enum grows a third member — SecItem (macOS),
   **Secret Service/libsecret** over D-Bus (Linux; the `keyring` crate or
   `secret-service` directly), file (dev, both platforms — it already exists
   via `NEO_KEYCHAIN_BACKEND`). `Secret` zeroization, redaction and the
   `expose()` audit points are backend-independent and unchanged.
2. **neo-ax** becomes an explicitly macOS-only crate: `cfg(target_os =
   "macos")` at the workspace edge, and on Linux `neo app` / `neo ax` / the
   `app` tool refuse with one honest sentence and a pointer at the browser
   path (the TUI's "not built yet" voice, not a compile error and not a stub
   that pretends).
3. **neo-voice**: `cpal` capture is portable (ALSA/Pulse); gate Apple STT and
   the TCC permission module; on Linux the transcriber table is
   OpenAI-only and `doctor` says so. `build.rs` Info.plist embedding is
   macOS-only already.
4. **src-tauri**: gate `macos-private-api` and the `tauri-nspanel` git dep
   behind the macOS target (nspanel is spike-only anyway — Surfaces audit).
   No Linux Tauri work in Q5 (L2 deferred); it merely may not poison the
   build graph.
5. **Paths and discovery**: data dir and the Q1 persistent Chrome profile go
   through XDG base dirs on Linux; Chrome discovery checks
   `google-chrome`/`chromium` on `$PATH`; `doctor` rows become
   platform-aware (no TCC rows on Linux, a D-Bus/Secret Service row
   instead). The screen lease is `rustix` flock — already portable.

L0.1 is a one-day audit producing the exact `cfg` map (which modules, which
deps move to `[target.'cfg(target_os = "macos")'.dependencies]`) before any
gating lands, so the seams are cut once, in the right places.

### 8.3 CI and acceptance (L1)

- **Ubuntu lane** beside macos-14: rust-cache, `clippy --workspace
  --all-targets -D warnings`, `cargo test --workspace` — same commands, the
  `cfg` gates do the narrowing. The lane is required, not advisory: Linux
  breakage must block merges or it will rot within a week.
- The Q3 **review set's browser tasks run on Linux** too (Chrome + CDP +
  Jev are identical); the AX fixture-app tasks stay macOS-tagged. Eval
  consensus machinery is unchanged.
- `deny.toml` and the toolchain grow the `x86_64-unknown-linux-gnu` /
  `aarch64-unknown-linux-gnu` targets (the Intel-macOS lesson from the
  audit: a declared-but-never-built target is a fiction — declare only what
  CI builds).

**Acceptance Q5:** a stock Ubuntu box goes `git clone` → `just setup` →
`neo doctor` (every unmet row names its one command) → `neo tui` → a
browser goal completes headed, with confirm cards working — the Q4
two-minute bar, on Linux. README's Requirements section gains the Linux
paragraph the same day (B6).

### 8.4 What Linux does *not* get yet, and why that is honest

- **Native-app control (L3)**: the macOS AX design does not transplant.
  AT-SPI2 (via the `atspi`/zbus crates) can back the same `Observer` seam
  jev-nav already defines — that seam is the right shape, and nothing in Q5
  may narrow it — but coverage across GTK/Qt/Electron is uneven and
  synthetic input on Wayland is restricted (libei/portal territory), which
  is exactly the kind of half-working surface B2 forbids. It returns as a
  demand-gated milestone with its own fixture app and review-set tasks, or
  not at all.
- **Voice beyond STT, desktop shell**: inherit C2/C3 unchanged.

---

## 9. Amendments to ratify in 00-decisions

| # | Amendment | Replaces |
|---|---|---|
| A-Q1 | M4 becomes **M4-lite**: single live task lifecycle with confirm/ask/resume; FIFO queue, intake heads, router and lanes deferred to demand | 03 §1-2 scope for M4 |
| A-Q2 | Drop the ten never-read tables in the Q2 migration; tables are created when first read, not in advance | 05 §4.3 "full schema up front" |
| A-Q3 | Default browser = persistent **headed** managed profile; headless is the exception | current code behavior (10 §10 already says this — this ratifies enforcement) |
| A-Q4 | Voice remains push-to-talk + STT hints through Q4; plan-02 pipeline is post-Q4 | 02 sequencing |
| A-Q5 | Decide: `AXEnhancedUserInterface` allowed per-app with reset-at-task-end, or never (revert `actor.rs:560-583`) | 01's "never set" vs code |
| A-Q6 | Decide: Apple on-device STT stays the keyless default (amend K6) or OpenAI-only returns | K6 vs `stt/mod.rs` |
| A-Q7 | Wire timeout is 5 s; safety heads fail closed; head-shape mismatch is a hard error | resolves 10 §3 *(verify)* |
| A-Q8 | Linux is a supported target at tier L1 (TUI + browser product); native-app control on Linux is demand-gated (L3); declared build targets are exactly what CI builds | 05 platform scope (macOS-only) |

---

## 10. Sequence and dependencies

```
Q0 stop the bleeding ─▶ Q1 browser feel ─▶ Q2 no dead ends ─▶ Q3 verification ─▶ Q4 feel
  (days)                 (1–2 weeks)        (2–4 weeks)         (1–2 weeks)        (1 week)
                           │                   │                    │
                           │                   │                    └─▶ Q5 Linux (L0 seams may start after Q0;
                           │                   │                        L1 gate = review set + cards, so after Q2/Q3)
                           └── Q2 needs Q1's persistent tab (resume) and the metal.rs pre-factor (5.6)
```

Q0 has no dependencies and starts immediately. Q3's CI lanes can begin in
parallel with Q2 (the neo-cdp transport tests need nothing from it). The
review set is drafted during Q1 so Q2 is built against it, not graded after.
Q5's L0 seam-cutting (`cfg` gates, keyring backend, XDG paths, Ubuntu lane)
is mechanical and can proceed in parallel from Q0 onward; its L1 acceptance
deliberately waits for Q2's cards and Q3's review set so Linux ships the
*good* product, not a preview of the old one.

## 11. Risks

| Risk | Control |
|---|---|
| Fail-closed (Q0.1) makes runs pause "too often" before Q2's confirm card exists | Interim: pause renders as today's Blocked text with the head named; annoying-but-safe beats silent-but-unsafe for the days between Q0 and Q2 |
| Resume tokens go stale in long confirms | Resume = re-observe + re-guard, never replay; the stale machinery already exists (`lib.rs:210-221`) and Q2 leans on it |
| Headed persistent Chrome surprises tests/CI | `--headless` stays for eval/CI; only the *user default* flips |
| Table rebuild migration bricks a dev DB | Same discipline as messages-v4: data-preserving rebuild + seed-fixture test + backup-before-migrate already in neo-store |
| Scope creep back toward the full 03 pipeline | C1–C4 are ratified amendments; anything beyond M4-lite needs a new decision, not momentum |
| Linux lane rots (advisory-green syndrome) | The Ubuntu lane is required from the first day it exists; `cfg` seams are cut once per the L0.1 audit map, not ad hoc |
| Wayland/AT-SPI half-support tempts a shipping shortcut | L3 is demand-gated by amendment A-Q8; `neo app` on Linux refuses honestly rather than half-working (B2 applies to platforms too) |
