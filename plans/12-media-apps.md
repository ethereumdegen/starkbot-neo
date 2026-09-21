# 12 — Media through apps: Diffusion Studio and Powermove edit, Degen Media Studio generates

Dated 2026-09-19. Decided by the user: Starkbot authors and edits media by
**operating media apps**, not by calling generation APIs itself. It uses Jev
and accessibility, the same way it drives any website or native app. There
are three apps:

| App | Role |
|---|---|
| **Diffusion Studio** | editor: infinite canvas + timeline video editor (MPL-2.0; macOS app + web) |
| **Powermove** | editor: AI-native motion and video editor for macOS whose own agent can add or fork panels, effects and workflows (GPL-3.0; Electron + Svelte) |
| **Degen Media Studio** (DMS, formerly Degen Media Maker) | **generator only**: fal.ai stills, edits and motion, QuiverAI vectors, as takes with lineage, ready to import into the editors ([13](13-degen-media-studio.md)) |

Starkbot therefore needs **only an OpenAI key and a Jev (TypeSafe) key**. It
holds no fal or QuiverAI key. DMS owns those. The editors own whatever
credits or agent logins they use.

This supersedes the parts of [07-media](07-media.md) that embed the DMM
library, and it **retires** [11-hypercanvas](11-hypercanvas.md): canvas and
timeline editing live in Diffusion Studio and Powermove. The
decision-record changes are K1′, A12′, A13′, A19, A21 and S8 in
[00-decisions](00-decisions.md).

## 1. Why apps instead of APIs

- **Two keys, not four.** Onboarding stays OpenAI plus Jev. Generation
  keys and bills stay with the tool that spends them.
- **The work stays editable.** Every edit lands in a real Diffusion Studio
  or Powermove project that the user can open and keep working on without
  Starkbot.
- **Best-in-class editors, not a homegrown one.** Two open-source editors,
  each with its own agent story, beat a canvas we would have to build.
  Starkbot's job is to operate them well (the product's focus P2).
- **Safety stays uniform.** Generating, exporting, publishing and spending
  are clicks. The existing rules, safety heads and confirm cards cover them.

## 2. The apps, as Starkbot sees them

| | Diffusion Studio | Powermove | Degen Media Studio |
|---|---|---|---|
| Starkbot observes via | macOS app: `AxObserver` (**first**, user decision); web app later: CDP snapshot | macOS app (Electron): `AxObserver` with `AXManualAccessibility` on; alternatively `powermove serve` → its web UI in managed Chrome via CDP | local web UI at `127.0.0.1:7788` in managed Chrome, via CDP |
| Typical work | import takes and footage, arrange on the timeline, text, keyframes, transitions, export | the same, plus motion work, and asking **Powermove's own agent** for a new panel, effect or workflow | shoot-outs, edits, motion, vectors, starring, "send to editor" |
| Credentials | optional Diffusion Studio credits (its AI features) | its agent needs the Codex CLI or Claude Code, installed and logged in by the user | `FAL_KEY`, `QUIVERAI_API_KEY` in DMS's Keychain |
| Read-only side channel (grounding only) | `dapi` | project state through its typed tool layer, if exposed (verify in S8b) | `dms` CLI / JSON op API |

**Policy:** the UI is the primary path. Side channels may be declared by the
pack only as **read-only grounding**, to verify that a step did what Jev
intended. Every mutation goes through the UI and the gates. P3 holds:
Starkbot never gets a shell, and the apps' CLIs are never a general tool.

### Powermove's self-rewriting agent (decision A21)

Powermove can change **its own code**: its agent writes, compiles and
hot-loads new panels and effects. Starkbot may *command* it by typing a
request into Powermove's agent panel ("add a panel that batch-applies the
brand LUT to selected clips"). That is a code-changing action on the user's
machine, run by another agent under the user's own Codex/Claude login, so:

- it is always a **confirm card** (the pack marks the agent panel's submit
  control `confirm_all`; the card quotes the request text);
- Starkbot never approves Powermove's own permission prompts for it. Those
  stay with the user;
- after it finishes, Starkbot only *uses* the new panel through the UI,
  like any other control, and the trace records that the app changed.

## 3. The Jev-enablement skill pack: `media-apps`

A pack in the [06-packs](06-packs.md) format with the neo-only `desktop/`
folder. It teaches Starkbot to work *with* these apps without per-site
scripts: P9 holds, and everything must keep working with the hints
removed, only more slowly and with more `BLOCKED` results.

```
media-apps/
  pack.toml                 id, version, the three apps, no requires_env
  vocabulary.md             glossary appended to Jev's rules: composition, layer, clip,
                            keyframe, trim, split, transition, mask, caption, panel,
                            effect, take, star, shoot-out, send-to-editor, export preset …
  desktop/
    diffusion-studio.toml   technical hints only: bundle id, AX strategy, settle waits
                            after import and render, which dialogs are file panels
    powermove.toml          bundle id, AXManualAccessibility (Electron), waits after
                            hot-load and render, the agent panel's submit = confirm_all
    dms.toml                origin 127.0.0.1:7788; waits after generate (aria-busy)
  routines/                 Jev-verified fixed step lists (run without Sol):
    ds-import.toml          import → OS open panel → pick → verify in the media bin
    ds-export-mp4.toml      export → preset → confirm → wait → verify file
    pm-import.toml, pm-export-mp4.toml
    dms-shootout.toml       prompt → models → aspect → generate (confirm) → wait → sheet
    dms-send-to-editor.toml star → "send to editor" → note the exported path
  goals/                    Sol decomposition templates: a brief becomes navigate(goal)s
  policy.toml               tighten-only: generate / render-with-credits / export-overwrite /
                            publish / Powermove agent submit ⇒ confirm_all
  grounding.toml            optional read-only probes: "project has N clips",
                            "export exists, duration d", "take t0004 exported to …"
```

**Enabling it collects no keys.** It checks that each app is present,
runs `neo doctor media-apps` (Jev reads each app's main view once), and
offers one-click navigate goals that stop at each app's own
key/credits/login settings, where the user types their own keys. It ships
in every build, **disabled**.

## 4. How a media task runs

```
brief ─▶ intake route: media ─▶ Sol: art direction + plan
  navigate("In Degen Media Studio, run a shoot-out: <prompt>, nano-banana + flux-ultra + ideogram, 9:16")  ← confirm (spend)
  look(sheet)                                   ← Sol vision on the contact sheet DMS produced (P10 exception)
  navigate("In Degen Media Studio, star t0003, animate it with kling, send both to the editor")          ← confirm (spend)
  navigate("In Diffusion Studio, import the two exported files, build a 10 s 9:16 cut: …")
  navigate("Export 1080x1920 MP4 to ~/Movies/acme/")                                                    ← confirm (overwrite)
  (or the same edit in Powermove; or "ask Powermove's agent for a beat-sync panel" ← confirm, A21)
```

- **Hand-off between apps is files.** DMS's "send to editor" exports the
  chosen takes (original files plus a sidecar JSON with prompt, model and
  lineage) to one folder per studio (`~/Movies/Degen Media Studio/<studio>/`).
  The editors import from there through their own import dialogs. Starkbot
  never moves files itself (P3).
- **Seeing the work.** Sol critiques only renders the apps produced: DMS
  sheets and filmstrips, and exported frames or a preview of an editor's
  canvas (CDP screenshot for web UIs; for native apps, the exported file's
  frames). Never used to find where to click (P10).
- **Spend.** The `spends` head plus `confirm_all` on generate and render
  controls turns every paid action into a confirm card showing the app's
  own cost text. DMS puts a price estimate in every generate button's
  accessible name.

## 5. Smoke tests (spike S8, before M6′)

All run from the headless `neo` CLI with **only** `OPENAI_API_KEY` and the
TypeSafe key in Starkbot's Keychain. Report to `plans/spikes.md`.

**S8a: Diffusion Studio macOS app, "a 10-second vertical promo"** (user: macOS
app first). Needs `neo-ax` + `AxObserver` at least in spike form ahead of
M10. First record which timeline and canvas elements the app exposes to
accessibility.
- Fixtures: three short clips + a logo PNG in `fixtures/media/`.
- Brief: *"Make a 10-second 9:16 promo from these clips: logo over the first second, title
  'Loud on purpose' fading in at 2 s, dissolves between clips, export an MP4."*
- **Pass:** the MP4 exists; ffprobe shows 1080×1920 and 10 s ± 0.3 s; the title
  is visible at 3 s and 8 s (Sol vision on a filmstrip, plus a human
  spot-check the first time); passes 4 of 5 runs; no fal or Quiver key
  anywhere in Starkbot. **Record:** Jev requests, Sol calls, `BLOCKED` count,
  wall time, median step latency.

**S8b: Powermove, "the same promo, then a new panel"**
- The same brief as S8a in Powermove (AX first; compare with `powermove serve` via CDP).
- Then: *"Ask Powermove's agent to add a panel that applies a 2-second fade to every
  selected clip, then use it on all clips."* The request passes a confirm
  card; Starkbot uses the new panel through the UI.
- **Pass:** MP4 checks as in S8a; the panel appears and is operated by Jev;
  the confirm card fired exactly once for the agent request and once for
  export.

**S8c: Degen Media Studio → editor, "generate, then cut"** (needs DMS's UI, milestone G3 in 13)
- Brief: *"Three 9:16 stills of a chrome microphone on black velvet; pick the best; animate
  it; put the still and the clip into Diffusion Studio and cut a 6-second teaser."*
- **Pass:** a shoot-out of 3 takes; one starred by Sol vision; a motion
  take; both sent to the editor folder with sidecars; imported and cut in
  Diffusion Studio; every generate click passed a confirm card quoting
  DMS's estimate; Starkbot made zero fal or Quiver calls itself.

**Acceptance for M6′:** S8a and S8b each pass 4 of 5 runs with no stale or
occluded clicks and no unconfirmed paid or code-changing actions.

## 6. What changes in Starkbot

- **Removed:** `neo-media`, `neo-canvas`, `neo-canvas-agent`, `MediaBackend`,
  the fal/Quiver enablement flow, the Studio editor, Design mode's own
  canvas, and Jev micro-edits on our own document (the old A20).
- **Kept:** Sol's art direction, the quality pipeline (brief → per-model
  direction → shoot-out → vision critique → targeted edits → finish →
  export presets), now executed as navigate goals across the three apps.
  `look` on app-produced renders stays, as do spend caps (counted from
  confirms) and the take vocabulary (DMS's).
- **Design mode** is retired as a separate surface. Media work shows in the
  normal Assist view (Conversation · Queue · Mind) while the apps run in
  their own windows.
- **Onboarding** (04-ui, 08-providers): no media-keys step.

## 7. Risks

| Risk | Control |
|---|---|
| An editor draws its timeline on `<canvas>` with no accessibility | S8a/S8b check this first. Fall back to inspectors, menus and keyboard shortcuts. Both apps are open source, so send missing ARIA/AX upstream. Powermove's `serve` web UI gives the CDP path as an alternative. |
| Electron AX tree off by default (Powermove) | `AXManualAccessibility` hint in `powermove.toml`, cleared on exit (as for other Electron apps, 01-accessibility) |
| Powermove's agent changes the app mid-task | A21: confirm per request; the trace notes the app changed; routines re-verify with Jev after a hot-load |
| App UI churn breaks routines | routines are Jev-verified and fall back to plain navigation; no selectors (P9) |
| Hidden app-side spend | confirm_all on paid controls; DMS quotes estimates; editor credit text read into the card |
| Long renders look like a hang | per-app settle waits; WAIT is not counted as stuck; grounding polls the export |
