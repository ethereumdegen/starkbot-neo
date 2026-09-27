# 19 — Organic environments: closing the cave gap (degen-modeler + degen-paint)

Dated 2026-09-27. Triggered by the first environment build (the dark-rock
cave, plan 18 addenda) judged against a reference image of a stylized
cave: big faceted rock masses, baked directional light + glowing crystals,
moss on upward faces, cobbled floor, water, one continuous space.

## 1. Diagnosis (what actually went wrong)

| Symptom | Cause | Where it lives |
|---|---|---|
| Texture reads as noise wallpaper | brush strokes were **scripted by an LLM** (random blotches/lines); no form-following facets, no crevice shadow, no lit planes | workflow: degen-paint used as a plotter instead of an image model with a reference |
| Visible repetition and seams | one 512² tile at 150 tex/m over ~25 m; no seamless wrap, no variation, no macro tint | degen-paint: no tileable mode; degen-modeler: no multi-sheet/detail blend |
| Flat, lifeless | **no lighting at all**: unlit material, no AO, no vertex shading; reference is mostly lighting | degen-modeler: no bake |
| Floor slices the dome, cones float, boulders intersect | kitbash of disjoint objects; nothing welds, unions, or checks contact | degen-modeler: no join/boolean/solidify; gate validates per object only |
| Mouth shows inside texture through a razor edge | shell has no thickness; double-sided hack | degen-modeler: no `solidify` |
| Lumps look wrong | proportional nudges on a radial lathe; wrong base topology for organic volume | degen-modeler: no subdivide/noise-displace/smooth |
| Critique missed the disconnection | it only sees exterior turntable renders | degen-modeler: no interior camera, no connectivity metrics |

## 2. Principles

- **Reference-driven.** A project carries reference images; generation,
  critique and review all see them. "Looks like the reference" becomes a
  measurable head.
- **Environments are one connected surface.** Shells, floors, formations
  become one manifold (or explicit contact), and the gate says so.
- **Light is baked, not hoped for.** Classic-MMO style = unlit texture x
  baked AO/vertex shading. Every export of an environment carries it.
- **Image models paint; brushes touch up.** degen-paint's fal ops with a
  reference and tiling are the texture source; the brush is for fixes.

## 3. degen-modeler — ops, rules, renders

### E1 Organic geometry
- `subdivide {object, levels, smooth}` — Catmull-Clark/loop, budget-gated
  (refuses if it would breach the class budget).
- `displace_noise {sel, amplitude, scale, seed, along: normal|axis}` —
  seeded fbm along normals; deterministic.
- `smooth {sel, iterations, factor}` — Laplacian with boundary pinning.
- `solidify {object, thickness}` — shell → two-sided volume; kills the
  double-sided hack and the razor mouth.
- `tunnel {object, path: [[x,y,z]…], radius, segments}` — swept tube
  with radius curve, the right primitive for passages; `cavern
  {…}` = noise-displaced ellipsoid with a floor cut. Both pre-seamed and
  UV-continuous like today's primitives.
- `join {objects, name}` + cross-object `merge_verts` → one mesh;
  `boolean {a, b, mode: union|subtract|intersect}` (BSP or mesh-arrangement)
  so formations grow *out of* the floor and boulders sit *in* the wall.
- `snap_to_surface {sel, target}` — plant stalagmites on the floor mesh.

### E2 Scene-level gate
- `scene.intersects` (Hard for environments): triangle-triangle
  intersection between objects not declared as contact.
- `scene.floating` (Warn): object with no contact within ε of any other.
- `mesh.open_boundary` (Hard for class `building`/`environment` shells
  unless the object is tagged `open`).
- `mesh.inverted` (Hard): faces whose normal points into a closed volume.
- New class `environment` with its own budgets (e.g. 12k tris) and a
  texel band per surface role (wall/floor/ceiling).

### E3 Baked lighting
- `bake_ao {object, samples, strength}` → per-vertex AO into `COLOR_0`;
  `bake_sun {direction, warmth}` → directional term into the same channel;
  exporter emits `COLOR_0`; renderer and viewer multiply unlit color by it.
- Emissive materials (`emissive: [r,g,b], strength`) for crystals/lamps →
  `KHR_materials_emissive_strength`; `bake_glow {light_objects, radius}`
  tints nearby vertex colors (the reference's blue rim light, cheaply).
- Vertex-color moss/dirt masks: `paint_vertex {sel, color, falloff,
  facing}` (upward faces get moss tint; crevices get dirt).

### E4 Texturing
- Multi-material environments: wall / floor / ceiling / accent trims per
  pack; `uv_role {object, role}` sets the band and default sheet.
- Macro variation: a second UV set or a 4-tile atlas with `uv_variation
  {seed}` rotating/offsetting islands so repetition breaks.
- Reference intake: `set_reference {path}` stores images in the project;
  critique and review send them alongside renders; a `reference_match`
  head (vision) joins the five.

### E5 Renders + critique for environments
- Interior views: `render/interior?object=` places cameras inside the
  shell (from the mouth looking in, from the back looking out, ceiling).
- Critique prompt variant for environments: asks about continuity,
  scale, lighting, focal points; contact sheet = exterior + interior.
- Metrics: `connectivity` (share of objects in contact), `lit_range`
  (value range of baked shading), `repetition` (autocorrelation of the
  rendered texture).

## 4. degen-paint — the texture source

- **Tileable mode**: `doc.set-tiling on` wraps every stroke/filter across
  the borders and renders a 2x2 seam preview; `raster.filter.seamless`
  (offset-and-inpaint) for imported images.
- **Generate from reference**: `ai.texture.generate --reference <img>
  --tile --prompt …` (fal image-to-image / kontext with the reference as
  style) producing wall/floor/moss sheets in one call; `ai.image.inpaint`
  for seams and repetition hotspots.
- **Bake helpers**: `raster.filter.height-to-shading` (fake lit planes
  from a height map, top-left light), `raster.filter.crevice-darken`,
  palette-lock (`raster.color.quantize --palette pack`) so results stay
  in the pack's colors.
- **Batch strokes**: `raster.paint.strokes` taking an array — 300 process
  spawns became the slowest part of today's loop.
- **Rock brushes**: alpha-stamped brush tips (chisel, facet, speckle)
  with texture, so a touch-up stroke reads as rock, not marker.

## 5. Milestones

| M | Done when |
|---|---|
| E1 | cave rebuilt as one joined manifold via `cavern` + `tunnel` + `solidify` + `join`; `dgm replay` clean; gate green with the new scene rules on |
| E2 | `scene.intersects`/`floating`/`open_boundary` catch today's cave as-is (regression fixture) |
| E3 | baked AO + sun + crystal glow visible in the viewer; export carries `COLOR_0` + emissive; Khronos clean |
| E4 | wall/floor/moss sheets generated by fal from the reference in degen-paint tiling mode; repetition metric drops below threshold |
| E5 | `dgm critique` with interior views + reference scores the rebuilt cave above the old one on `reference_match` and `style_fit` |

## 6. Risks

| Risk | Control |
|---|---|
| Booleans are hard to make robust | start with join + merge_verts + snap_to_surface (covers 80%); boolean behind a feature flag with a fixture suite |
| fal costs in loops | degen-paint's budget/quote gate already exists; reference-driven generate is one call per sheet |
| Vertex colors bloat GLBs | u8 normalized `COLOR_0`; optional |
| Subdivision breaks the low-poly promise | budget-gated; one level max by default |
