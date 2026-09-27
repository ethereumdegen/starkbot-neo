# 18 — Degen Modeler: agent-first glTF modeling, in Rust

**Status 2026-09-26: v0 built** at `~/ai/degen-modeler` (M0–M3 + M5 core,
M4 viewport landed; workspace tests green; smoked end-to-end — barrel over
the API and a rigged/animated character over the CLI, both Khronos-valid,
replay byte-identical). Deviations from this doc, code authoritative:
indexed face-set kernel with derived adjacency instead of half-edge (same
op surface); the local CLIP scorer is not built — reviews report the tier
as skipped; turntable ships as a filmstrip PNG, not webm; `uv_project`
wire is flat `kind`+`axis` (serde flatten/deny conflict); gate gained
`uv.missing_sheet` (Hard) and `uv.missing_texture` (Warn); vision pricing
constants cover the default models only.

**2026-09-27, tree + degen-paint round trip.** Driven as an agent
end-to-end: low-poly tree (trunk/branch lathes + three crossed cutout
quads, 63 tris) modeled over the API; its atlas authored and brushed in
degen-paint (268 journaled ops: fills, gradient, ~190 `raster.paint.stroke`
dabs, erases) and wired in as a `File` texture — gate caught real
texel-band sins (branch 776 tex/m, apex fans 14) and the fixes went back
through ops until pass; export Khronos-clean. Added along the way:
`uv_assign_rect` (island layout into an arbitrary rect — the missing op
for owned atlases) and the `dgm-view` binary (simple Bevy glTF viewer:
orbit + turntable, auto-framing). fal generation in degen-paint is wired
and one credential away (`FAL_KEY`; `dpaint doctor` confirms) — no
software gap. Known rule gaps for a later pass: `uv.waste` is per-object
and over-reports on multi-object shared atlases (should judge per atlas);
the CWD gap and the placement gap below were closed the same day.

**2026-09-27, one-shot upgrade round.** Driven by a real `dgm critique`
run (new tool: `POST /critique` / `dgm critique` — renders + atlas +
digest to the vision model, OPENAI_API_KEY required, suggestions answer
in op names): primitives now ship **meter-scaled default UVs with
fan/cap seams and non-overlapping island lanes**, killing the
cylindrical-projection-of-a-cone failure class at the source;
`uv_assign_rect` gained an optional `texels_per_meter` cap so rect
fitting can never leave the pack band; `File` textures resolve against
the project root (pack parent), not the CWD; the agent SKILL gained a
"one-shot playbook" section encoding the session's learned recipes
(material-early ordering, owned-atlas partitioning with gutters,
revolved-shape fan handling, cutout foliage, density-fix guidance).
Critique v1→v2 on the tree confirmed the fixes: the false "missing
textures" high-severity issue vanished once renders resolved textures.
Still open: `uv.waste` should judge per shared atlas, not per object
(both the gate and the critique model over-flag it); a UV
translate/rotate op remains the missing fine-placement tool.

Dated 2026-09-26. Drafted for decision:

- **New sibling app** in **`~/ai/degen-modeler`** (CLI `dgm`), a Rust
  workspace. Not a Starkbot crate; Starkbot and Omp are *clients*.
- **Agent-first, human-second.** The primary surface is a local HTTP API on
  `127.0.0.1:7799`. The human UI is a **Bevy 0.19** app (bevy_egui panels)
  driving the *same op layer* — one vocabulary, one ledger, humans and
  agents interleave in it. Bevy is itself a target consumer, so the
  viewport is validation, not just preview.
- **One output:** glTF 2.0 (`.glb`), in a **classic-MMO style** — WoW-Classic-
  like: low poly, hand-painted-look textures, shared trim sheets/atlases,
  deliberate UV economy. The style is a measurable contract (§2), not a vibe.
- **Integrated Jev harness** for cheap validation. TypeSafe takes text, not
  images, so perception is quantified first: the deterministic gate, render-
  derived metrics and a local perceptual scorer produce numbers; one TypeSafe
  request per checkpoint fuses them into fixed review heads. A **vision
  reviewer** on the user's inference connection (OpenAI-compatible or
  Anthropic vision, the K6 pattern) judges the actual renders — sparingly:
  it costs real money, Jev costs ~nothing.
- **No texture generation.** Trim sheets ship in the style pack; new textures
  arrive from Degen Media Studio / degen-paint as files with sidecars
  ([13](13-degen-media-studio.md)). Degen Modeler maps geometry onto them.

## 1. What it is

Blender answers "what can a human sculpt?". Degen Modeler answers "what can
an agent *reliably* build and verify?": a closed, semantic op vocabulary; a
scene an agent can fully introspect as JSON and renders; a validator that
says *pass/fail with element ids* instead of leaving quality to taste; and a
classifier loop (~150 ms/step) so an agent like Omp iterates
model → review → fix dozens of times before a human or Sol ever looks.

Every mutation is an **op** in an append-only ledger (`ops.jsonl`), with the
revision it applied to as parent. Replay is deterministic: the ledger *is*
the file format; `.glb` is an export of a revision, never the source of
truth. Undo is revert-to-revision; variants are branches.

## 2. The style contract (`packs/classic/`)

What "WoW-Classic-like" means, in numbers the gate can check:

| Axis | Contract |
|---|---|
| Triangles | per asset class: prop 50–800 · weapon 100–500 · building 500–4 000 · character 800–2 500; LODs at ~50 % and ~20 % |
| Textures | 256²/512², hand-painted look, lighting baked into paint; `KHR_materials_unlit` with a plain-lit fallback; alpha-test cutouts for foliage/rope |
| Sharing | one material per model where possible; UVs land on the pack's shared **trim sheets** (wood, stone, metal, cloth, foliage) or a per-project atlas |
| UV economy | mirrored/overlapped islands encouraged and *declared*; texel density inside a per-pack band (default 2–4 px/cm at 512); ≤ 15 % wasted atlas area on owned atlases |
| Silhouette | triangles spend on outline, not surface; interior detail is paint |
| Rig | ≤ 40 bones, ≤ 4 influences/vertex (the glTF limit), loop clips (idle, walk) |

A pack is data: budgets, palette, trim sheets, reference boards the local
scorer (§6) embeds for the `style` head. The classic pack's sheets are
**original hand-painted-alikes**
— no Blizzard asset ships or trains anything.

## 3. Architecture

```
degen-modeler/
  crates/dgm-mesh     half-edge kernel; topology ops; validators (manifold,
                      degenerates, budgets)
  crates/dgm-uv       seams, unwrap, projections, mirror/overlap sets,
                      texel density, atlas packing
  crates/dgm-scene    scene graph, materials, skeleton + skinning, animation
                      channels, selections, the op ledger (ops.jsonl)
  crates/dgm-atlas    shared texture library: trim sheets, palettes, region
                      registry, usage index (which models use which region)
  crates/dgm-render   CPU rasterizer (z-buffer, unlit, nearest sampling):
                      deterministic measurement views — beauty, wireframe,
                      UV-overlay, texel-density heatmap, 8-view contact
                      sheet, silhouette masks, turntable filmstrip — same
                      pixels everywhere, headless, golden-testable
  crates/dgm-gltf     export/import glTF 2.0 + KHR_materials_unlit,
                      KHR_texture_transform; golden-file tested
  crates/dgm-jev      the review stack: TypeSafe client (the jev-nav wire),
                      review heads, thresholds; the local perceptual scorer
                      (small ONNX CLIP/SigLIP over dgm-render views); the
                      vision-reviewer client (OpenAI-compatible / Anthropic
                      vision); the deterministic gate lives with the data it
                      checks (mesh/uv/scene) and reports through here
  crates/dgm-api      axum on 127.0.0.1:7799: POST /op, GET digests/renders/
                      artifacts, POST /review, sessions
  crates/dgm-ui       Bevy 0.19 viewport + bevy_egui panels over the same
                      op stream; skinned clip playback; op ticker +
                      revision graph showing agent activity live
  crates/dgm-cli      `dgm`: new/serve/op/check/review/export/skill --install
  packs/classic/      trim sheets, palette, budgets, reference boards,
                      rig presets + clip templates
```

`dgm-jev` speaks the same TypeSafe wire as `jev-nav` but shares no code with
Starkbot. Keys live in Keychain service `com.degen.modeler` (env fallback):
`TYPESAFE_API_KEY` for Jev, plus one optional inference key for the vision
reviewer. Keys reach clients only through headers, never argv.

## 4. The op layer: one vocabulary for humans and agents

An op is JSON — name, params, target selection — validated, applied,
appended. Families:

- **primitive**: box, cylinder, plane, lathe, ngon-prism;
- **topology**: extrude, inset, bevel, loop_cut, bridge, merge_verts,
  dissolve, mirror, flip_normals, decimate_to_budget (also makes LODs);
- **deform**: translate/rotate/scale on selections, proportional falloff,
  lattice, snap_to_grid;
- **select**: named, saved selections; semantic queries — `faces_facing +Z`,
  `boundary_loop`, `by_material`, `island_of <face>` — never pixel picks;
- **uv**: mark_seams, unwrap, project (planar/box/cylindrical),
  assign_to_trim(region), declare_mirror_set, set_texel_density, pack;
- **material**: bind atlas/trim region, unlit color, alpha mode;
- **rig**: preset-first — `apply_rig(preset, fit)` fits a pack skeleton
  (biped, quadruped, prop-hinge, tail/banner chain) to the mesh;
  `auto_weights` (distance falloff), `paint_weights(sel, bone, value,
  falloff, add|replace|smooth)`; `add_bone` only extends a preset chain;
- **anim**: preset-first — `apply_clip(template)` retargets the pack's
  normalized idle/walk/sway curves onto the fitted rig; keyframe ops tweak
  from there; loop markers, bake;
- **scene**: instance, LOD chain, name, tag, export.

Every op returns `{revision, diff summary, validation report}`; a rejected
op names the offending element ids and the rule. No op takes screen or
pixel coordinates — the API is closed over semantics, which is what makes
Jev's job (and Omp's) bounded.

## 5. Introspection: what an agent can see

- `GET /scene` — digest: element counts vs. budgets, bounds, materials,
  atlas occupancy, mirror sets, rig summary, current warnings;
- `GET /render/{sheet|turntable|wireframe|uv|heatmap}` — stable artifact
  URLs (the DMS contact-sheet convention, so Sol can `look`);
- `GET /query` — measurements, element lookup, raycast pick along a ray in
  model space (for "what is this lump" questions);
- `GET /ledger` — the op history with revisions and branches.

## 6. The Jev harness

Four tiers, in the jev-nav shape — rules first, cheap classifier for the
loop, a vision model only at escalation points, and no classifier is ever
load-bearing for safety:

**Deterministic gate** (free, every op; hard-fails block export):
non-manifold/degenerate geometry, tri budget, texel density out of band,
UV overlap outside a declared mirror set, atlas-region misuse, unweighted
vertices, > 4 influences, animation channels off the rig. Attribution is
exact: the failing element ids come back with the op result.

**Perception metrics** (local, free): TypeSafe takes no images, so every
perceptual question is turned into a number before Jev is asked anything.
From `dgm-render` views and the bound textures, deterministically: seam
contrast sampled across each UV seam edge; UV stretch/waste; triangle
density against surface curvature; silhouette-mask drift across LODs;
texture palette distance from the pack palette. From the **local scorer**
(a small ONNX CLIP/SigLIP, no network, no cost): each contact-sheet view
scored against the session goal text ("a wooden barrel") and against the
pack's reference board.

**Jev checkpoint** (~150 ms, one request, text only): state = the session
goal, the scene digest, the gate report and the §6 metrics; heads, each 0–1:

| Head | Fused from |
|---|---|
| `silhouette` | scorer goal-match per view, mask drift, tri-density spread |
| `style` | scorer reference-board match, palette distance, texel band fit |
| `seams` | seam-contrast maxima, stretch outliers |
| `waste` | UV occupancy, density-vs-curvature clusters, budget headroom |
| `done` | everything above plus the goal — "stop iterating?" |

Modes: on demand (`POST /review`), auto every N ops (the agent loop), and a
soft gate on export (report attached; only the deterministic gate blocks).
Below-threshold heads return the metrics and element ids that drove them
(seams → the exact UV edges, waste → the densest cluster) and the views
behind those metrics, for the caller's own eyes. A failed or slow Jev call
degrades to metrics-only —
feedback is advisory; the gate is not.

**Vision review** (a vision LLM on the user's inference connection; seconds
and cents, not milliseconds): sees the actual images — contact sheet, UV
overlay, close-ups of flagged regions — plus the goal and the pack's
reference board, and returns the same five heads with prose notes per view.
It runs only at escalation points, never per op: on demand
(`POST /review?tier=vision`), when Jev's heads sit in a configured
low-confidence band or contradict the scorer, and once before export when
`done` first clears. Without an inference key the harness runs
metrics + Jev only and says so in every report.

The point: Omp loops build → review → fix at classifier price, spends a
vision call at milestones, and asks a human or Sol to look only when `done`
clears.

## 7. The human UI (Bevy)

Two render paths on purpose: `dgm-render`'s CPU rasterizer measures
(deterministic pixels for metrics and golden tests, no GPU, no window);
the Bevy viewport interacts. Bevy is also a consumer of the exported
format, so what the viewport shows is what a game gets — unlit materials,
alpha-mask cutouts, skinning included.

- Bevy viewport: orbit/pan camera, element picking that resolves to ids
  (never pixels on the wire), gizmos; edits issue the same ops;
- **skinned clip playback**: idle/walk preview on the fitted preset rig,
  straight from the doc's clips;
- **engine preview** mode: loads the last exported `.glb` through Bevy's
  own glTF loader — the M5 check, permanently in the app;
- bevy_egui panels: the **op ticker** and revision graph show an agent
  working live; a human can grab the model mid-run — both write to one
  ledger, ordered; the review panel shows the last checkpoint's views,
  metrics and head scores;
- controls carry accessibility semantics (the [13 §4](13-degen-media-studio.md)
  rule), so Starkbot can drive Degen Modeler like any other app;
- Bevy is pinned to **0.19**; the op layer keeps the UI thin, so an engine
  bump touches `dgm-ui` only.

## 8. Laws

- **Ledger:** revisions strictly increase and are never reused; append never
  rewrites an earlier line; every op's parent is an earlier revision;
  replaying `ops.jsonl` reproduces a byte-identical `.glb`.
- **Export:** a `.glb` exports only from a revision that passes the
  deterministic gate; the validation report rides along in glTF `extras`.
- **Budgets:** an op that would breach a hard pack budget fails closed,
  naming the overage; nothing "fixes it later".
- **API:** binds `127.0.0.1` only, Host-checked; no token, no remote, ever.
- **Review:** advisory only — no op and no export blocks on a classifier or
  vision answer, or on either's outage.
- **Spend:** the vision reviewer runs only at the §6 escalation points, is
  priced per call in the report, and never retries a paid call on its own.

## 9. Milestones

| M | Name | Done when |
|---|---|---|
| M0 | Kernel | mesh + ops + ledger + replay; `.glb` export golden-file tested; `dgm check` runs the gate headless |
| M1 | UV + atlas | unwrap/project/pack, trim binding, mirror sets, texel density; classic pack v1; the full deterministic gate; §8 laws proven |
| M2 | API | axum surface, digests, renders, artifacts; **Omp builds a barrel headless end-to-end** from prompt to passing `.glb` |
| M3 | Jev | metrics + local scorer + review heads live with measured latency/cost; auto-checkpoints; vision reviewer at escalation points; a logged run showing iterate-until-`done` beats iterate-blind on op count |
| M4 | UI | Bevy viewport + ticker + review panel; skinned clip playback; engine-preview loads the exported `.glb`; human edit interleaved with an agent run lands in one ledger |
| M5 | Character | preset biped fitted + auto/painted weights + retargeted idle/walk clips + LOD chain; one character `.glb` validates in three consumers (Bevy, three.js gltf-viewer, Blender import) |

## 10. Risks

| Risk | Control |
|---|---|
| The local scorer may not discriminate at this style/resolution (hand-painted 256/512, low poly) | benchmark CLIP vs SigLIP on the pack's reference board before M3; heads weight deterministic metrics first, scorer second; a low-confidence band escalates to the vision reviewer, which actually sees the renders |
| Vision-review cost creep in agent loops | escalation-point-only by law (§8 Spend); per-call price in every report; Jev + metrics remain the inner loop |
| No inference key | vision tier is optional: metrics + Jev still run, reports say the tier was skipped |
| Half-edge kernel scope creep (sculpting, subsurf, booleans) | the op set is closed; the style contract makes high-poly tooling pointless; booleans deferred until a milestone needs them |
| "WoW-like" IP exposure | the contract is measurable properties (§2), never Blizzard assets; pack art is original |
| Texture provenance | none made here — pack sheets ship, new ones import from DMS/degen-paint with sidecars |
| `KHR_materials_unlit` support gaps in consumers | every export also carries a baked plain-lit fallback material; M5 checks three consumers |
| Bevy churn + compile weight | pinned to 0.19; UI stays thin over the op layer (engine bump touches `dgm-ui` only); measurement renders never depend on the GPU path |
