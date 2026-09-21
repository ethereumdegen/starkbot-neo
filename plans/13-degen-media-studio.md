# 13 — Degen Media Studio: generation only, in Bend 2

Dated 2026-09-19. Decided by the user:

- **Rename** `degen-media-maker` to **Degen Media Studio** (DMS; CLI `dms`).
- **Rewrite it in Bend 2** in the new sibling folder
  **`~/ai/degen-media-studio-bend`**. The Rust `~/ai/degen-media-maker` is
  left as it is until parity.
- **Thin it to generation.** It makes assets with fal.ai and QuiverAI and
  hands them to the editors, **Powermove** and **Diffusion Studio**, which
  do all editing ([12](12-media-apps.md)). No timeline, no canvas, no
  compositor. The Hypercanvas ([11](11-hypercanvas.md)) is retired, not moved.

An earlier draft of this doc (a Diffusion-Studio-shaped editor in Bend with
the Hypercanvas inside) is superseded. Its D0 compositing spike was built
and then dropped.

## 1. What DMS is

A generation studio for people and agents: every result is a **take**
(`t0001…`) in an append-only ledger with its prompt, model, request and
parents. Takes are reviewed on contact sheets, starred, and **sent to an
editor** as files with sidecar metadata.

| Keeps (from Rust `dmm`) | Drops (editing now lives in Powermove / Diffusion Studio) |
|---|---|
| `still` with `-m a,b,c` parallel shoot-outs · `edit` (image + instruction → image) · `motion` (image → video) · `cutout` · `upscale` · `run` (any fal endpoint) | the ad compositor (`ad`: layouts × formats, MP4 ads) |
| `vector` · `vectorize` · `vector-edit` · `animate` (QuiverAI Arrow, streamed) | `lockup` (icon + wordmark layouts) |
| model catalog + live fal schemas · aspect mapping per model dialect | `fonts` (was for ad typesetting) |
| brand kit as *prompt* style + palette (Recraft exact colours, Quiver palette instructions) | the Hypercanvas, comps, keyframes, rendering |
| `sheet` (contact sheet) · `look` (SVG on checker, video filmstrip) · `star` | |
| `keys` · `doctor` · `skill --install` · the preview board (becomes the UI) | |

New:
- **send to editor**: exports the chosen takes into
  `~/Movies/Degen Media Studio/<studio>/`. Each is the original file plus a
  `<take>.json` sidecar (prompt, model, parents, size, duration, cost).
  Optionally it converts to editor-friendly formats (SVG → PNG at a chosen
  size; motion → H.264 MP4).
- **quotes**: every paid call is priced before it runs.

## 2. Architecture (Bend 2)

```
degen-media-studio-bend/
  deps/metalcraft → ../../metalcraft-bend/src      json · host (exec, clock, uuid) · http (curl)
  src/studio.bend      studio.json, brand kit, paths
  src/ledger.bend      takes.jsonl: append, read, lineage, star; laws
  src/keys.bend        FAL_KEY / QUIVERAI_API_KEY in Keychain service com.degen.media-studio
  src/fal.bend         queue submit → status_url poll → result download (sub-path endpoints
                       poll at the app path); live OpenAPI schemas
  src/quiver.bend      Arrow jobs; SSE stream through curl -N, partial SVGs to live/
  src/models.bend      catalog, per-model aspect dialects, prices
  src/quote.bend       price × count × size → Quote; the spend gate; laws
  src/ops.bend         the verbs above, shoot-outs via IO.fork/IO.join
  src/sheet.bend       contact sheets and filmstrips via ffmpeg (tile/xstack; SVG via Chrome or rsvg helper)
  src/handoff.bend     send-to-editor exports + sidecars
  src/server.bend      127.0.0.1:7788: static UI + POST /rpc (Base TCP server; local Host check)
  ui/                  the generation UI (React + TS, DRY components + hooks)
  bin/dms.bend         the CLI
  LAWS.bend · PROOF.bend
```

- **Keys** reach curl only through the child's environment (the
  `http.bend` pattern, `--variable` + `--expand-header`), never argv.
  `.env` and `~/.degen-media-maker/keys.json` are still read for migration.
- **Studio layout** stays readable by both versions: `studio.json`,
  `takes.jsonl` (same line format), `takes/`, `sheets/`, `live/`. Old `ads/`
  folders are left alone and ignored.

## 3. Laws (`LAWS.bend`, proven in `PROOF.bend`)

- **Ledger:** take ids strictly increase and are never reused; append never
  rewrites an earlier line; every parent of a take is an earlier take.
- **Spend gate:** a paid op runs only with a quote whose total fits the
  remaining per-call and per-day caps, or with an explicit confirm token.
  Without either, the result is `NeedsConfirm` and nothing is sent.
- **Quotes:** the quote for a shoot-out is the sum of its models' quotes;
  zero-count requests cost zero.
- **Aspect mapping:** the size sent to a model is one of that model's
  allowed dialect values, and the nearest to the request.
- **Hand-off:** send-to-editor exports exactly the selected takes, each
  with a sidecar naming its take id and parents.

## 4. The UI: built to be driven by Jev

Starkbot operates DMS through its accessibility semantics, so the UI must
expose them. This is a DMS requirement, not a Starkbot hint:

- prompt, model checkboxes, aspect, count and style fields are real
  labelled form controls;
- each take tile is a focusable element labelled `"take t0004 · still ·
  flux-ultra · 1080x1920 · starred"`, with Star, Edit, Animate, Vectorize and
  Send to editor as real buttons;
- every generate control states its quote in its accessible name
  (`"Generate 3 stills · est. $0.12"`), which Starkbot's confirm card quotes;
- running jobs expose `aria-busy` plus a progress bar, and Quiver's partial
  SVG streams into the tile;
- the contact sheet for the current shoot-out has a stable link, so Sol can
  `look` at it.

## 5. Milestones

| G | Name | Done when |
|---|---|---|
| G0 | Core | studio + ledger + keys + fal + Quiver clients; `still/edit/motion/cutout/upscale/run/vector/vectorize/vector-edit/animate` at parity with Rust `dmm` (same ledger lines) |
| G1 | See + quote | `sheet`, `look`, `star`; model prices; the quote + spend gate; the §3 laws proven |
| G2 | Hand-off | send-to-editor exports + sidecars; manual import into Powermove and Diffusion Studio verified |
| G3 | UI | the §4 generation UI on `dms serve`; **S8c** runnable |
| G4 | Agent surface | `dms skill`, read-only JSON op API for grounding, `media-apps` routines verified by Starkbot |

## 6. Risks

| Risk | Control |
|---|---|
| Bend is young (2.0.x; no ABI promise for effects) | effects are small C/JS twins in the shared `host/`; pin the Bend version; tests on both backends |
| Contact sheets without resvg/tiny-skia | ffmpeg `tile`/`xstack` for rasters and video; SVG rasterised by headless Chrome (the shared CDP client) or `rsvg-convert` if present |
| Editors' import formats | send-to-editor converts SVG → PNG and motion → H.264 MP4 on request; S8c checks both editors |
| Starkbot users without DMS keys | DMS is optional: Starkbot still edits in Powermove and Diffusion Studio with the user's own footage |
