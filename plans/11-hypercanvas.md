# 11 — Hypercanvas: one agentic design surface for web pages, ads, graphics and video

An infinite canvas in the main window's **Design** mode (A15). The same surface designs a landing page, an ad set, a social graphic, a logo sheet and a video. Most of the designing is done **by the agent**; the user steers by voice, chat, pins, knobs and direct manipulation. It absorbs the Studio: 07 remains the spec of the generation engine (`neo-media`), this doc is the spec of everything the user sees and edits.

**One rule shapes everything (A13): every frame is an HTML + CSS document.** Rust owns that document; a browser engine lays it out and paints it; the agent writes and rewrites HTML because that is what models are best at.

## 1. North star

Reference UX: the canvas of Claude Design — chat beside a canvas, live HTML as the design, element-anchored comments, model-generated sliders, direct text edits, a design system applied automatically, a handoff bundle for a coding agent. OpenPencil, Penpot and pen.dev are UX references for layers, inspector and agent-on-canvas streaming. None is a dependency.

| Taken from the references | Ours only |
|---|---|
| HTML/CSS is the design, not a picture of it | **voice first**: selection + viewport travel with every utterance |
| comments pinned to elements → **pins** | **Jev micro-edits**: a spoken tweak lands in well under a second with no LLM call |
| generated sliders → **knobs** | many frames on one plane; **Set** frames (master → placements) and linked breakpoints |
| direct text edit | **takes** as nodes with lineage; Board frames; the fal/Quiver engine one tool call away |
| design system ingestion, handoff bundle | **Video** frames with a timeline; per-author undo; local Rust core that also runs headless in `neo` |

## 2. Frame kinds

| Kind | Is | Size | Outputs |
|---|---|---|---|
| **Web** | a page or section; one document, N breakpoints (defaults 1440 / 1024 / 768 / 390) shown as linked views | width per breakpoint, height = content | HTML+Tailwind · React+TS · PNG per breakpoint · PDF · handoff bundle |
| **Graphic** | fixed-size artwork: social card, OG image, thumbnail, slide, icon | fixed W×H | PNG · JPG · WebP · PDF · SVG (pure-SVG frames only) |
| **Set** | one master document + linked placements (1:1, 4:5, 9:16, 16:9, 1200×628, …) | per placement | every placement in one export, named by preset |
| **Video** | ordered scenes + a timeline (keyframes, clips, audio, captions) | fixed W×H, fps, duration | MP4 · GIF · animated SVG (pure-SVG frames only) · poster PNG |
| **Board** | free area for takes: shoot-outs, references, moodboard, lineage arrows | free | contact-sheet PNG |

All five are the same document type; `kind` only changes which sidecar fields, lints, docks and exports apply.

## 3. Frame document format

A frame is a directory `canvas/frames/<fid>/` holding `frame.html` (the design), `frame.json` (everything that is not HTML), `ops.jsonl`, `pins.jsonl`.

```html
<!doctype html>
<html data-neo-frame="f3" data-kind="set" data-v="1">
<head>
  <link rel="stylesheet" href="../../tokens.css">          <!-- @layer tokens  (generated, read-only) -->
  <link rel="stylesheet" href="../../kit/kit.css">         <!-- @layer kit     (components) -->
  <style data-neo="frame">  @layer frame { .ad{display:grid;grid-template-rows:auto 1fr auto;padding:var(--k-pad)}
      .headline{font:var(--weight-800) var(--k-headline)/1.05 var(--font-display);color:var(--colour-text)} } </style>
  <style data-neo="edit">   @layer edit  { [data-n="n4"]{letter-spacing:var(--tracking-tight)}
      [data-scope="9x16"] [data-n="n2"]{justify-self:center} } </style>
  <style data-neo="knobs">  @layer knobs { :root{--k-pad:var(--space-8);--k-headline:var(--text-7xl)} } </style>
  <style data-neo="motion"> /* generated from frame.json timeline; never hand-edited */ </style>
</head>
<body data-n="n1" class="ad">
  <img data-n="n2" class="logo" data-take="t0042" src="../../../takes/t0042.png" alt="Degen Radio logo">
  <div data-n="n3" class="copy"><h1 data-n="n4" class="headline">Radio for degens</h1>
    <p data-n="n5" class="sub">Live charts, zero ads.</p></div>
  <a data-n="n6" class="cta" data-c="button" data-c-v="2" style="--c-tone:var(--colour-accent)" href="#"><span data-n="n7" data-slot="label">Listen now</span></a>
</body></html>
```

**Layers.** Fixed order `@layer tokens, kit, frame, edit, knobs;`. The agent's generative CSS lives in `frame` (class selectors). Every op-driven style change lives in `edit`, keyed by `[data-n]` and optionally a scope. Knob values live in `knobs`. This is what makes an op a small, invertible text edit and lets a rewrite replace `frame` CSS without destroying user tweaks. A `compact` transaction folds `edit` rules into `frame` when the agent is asked to tidy.

**`data-n` ids.** `n` + base-36 counter, unique per frame, allocated **only by Rust**, never reused, present on every element. The agent must keep `data-n` on nodes it keeps and omit it on new nodes (§5.4).

**Allowed HTML.** `html head body style link[rel=stylesheet, local] main header footer nav section article aside div span p h1–h6 ul ol li a img picture source figure figcaption video[muted,playsinline] svg(+children, no `script`/`foreignObject`) button input[type=text|email|search|checkbox|radio, inert] label select option table(+children) blockquote hr br strong em small sup sub code`. Attributes: `data-*` of ours, `class id href src srcset alt title role aria-* width height loading type placeholder value for colspan rowspan viewBox` + SVG presentation attributes. `style=""` may carry **custom properties only**; any other inline declaration is hoisted into a `frame`-layer rule on parse.

**Sanitisation (applies to every byte that enters a document: agent output, imports, paste).** Removed: `script`, `iframe`, `object`, `embed`, `base`, `meta[http-equiv]`, `form[action]`, every `on*` attribute, `javascript:`/`data:text/html` URLs, `srcdoc`. CSS removed: `@import`, `url()` that does not resolve inside the studio folder, `expression`, `behavior`, `-moz-binding`. Allowed at-rules: `@layer @media @container @supports @keyframes @font-face` (studio `fonts/` only). **No network at render time**: the webview frame gets a CSP of `default-src 'none'; img-src asset: data:; media-src asset:; font-src asset:; style-src 'unsafe-inline' asset:`; the export renderer fails every request outside the studio folder through CDP `Fetch` interception *(verify: `Fetch.enable` + `Fetch.failRequest` for `file:`-loaded documents)*. Fonts and images from the web are downloaded **by Rust** into the studio first, never fetched by a frame. `a[href]` is kept as data and is inert on the canvas.

**Tokens and knobs are CSS custom properties.** Tokens: `--colour-* --font-* --text-* --weight-* --leading-* --tracking-* --space-* --radius-* --shadow-* --ease-* --dur-*`. Knobs: `--k-<slug>`, declared in `knobs`, consumed in `frame`. Lint `off-token` fires on any raw length/colour in `frame` or `edit` that is not a `var()`, `0`, `%`, `fr`, `auto` or `1px` hairline.

**Takes.** `data-take="t0042"` on `img` / `video` / inline `svg`; `src` is a studio-relative path, rewritten by Rust if the take is replaced. Every take op (`regenerate`, `edit`, `cutout`, `upscale`, `animate`) yields a new take and one `PlaceTake` op, so undo restores the previous take.

**Components / instances.** A component is `canvas/kit/<name>.html` (one root, `data-slot` children, `--c-*` variables) + rules in `kit.css`. An instance is stored **expanded** (the browser needs no script): root carries `data-c="<name>" data-c-v="<version>"`, inner nodes carry `data-ci="<component node id>"`. Instance overrides are exactly two things: slot content and `--c-*` values on the root. `UpdateComponent` re-expands every instance, preserving slots, `--c-*` and `data-n`.

**Scopes (per-breakpoint / per-placement overrides).** The frame root gets `data-scope="<id>"` at render time. Web: a breakpoint override is `@media (max-width:767px){[data-n="n4"]{…}}` in `edit` — real media queries, because that is what ships. Set: a placement override is `[data-scope="9x16"] [data-n="n2"]{…}`. Master layout should reflow by itself (flex/grid, `clamp()`, `@container`, `cqw`); overrides are for what reflow cannot do (order, visibility, type step, crop focal point). Overrides may set: any style, `display:none`, `order`, `object-position`. They may **not** change text or structure — content is shared by definition.

**`frame.json`.**

```json
{ "id":"f3","kind":"set","name":"Launch ad","rev":412,"next_n":"n1k","size":{"w":1080,"h":1080},
  "breakpoints":[], "placements":[{"id":"1x1","w":1080,"h":1080,"preset":"ig-feed"},{"id":"9x16","w":1080,"h":1920,"preset":"ig-story"}],
  "knobs":[…§9…], "locks":["n6"], "regions":{"hero":"n12"}, "brief":"briefs/b07.md",
  "timeline":null, "lineage":{"variant_of":"f2","axis":"palette"} }
```

**Video.** `body` holds one `<section data-scene="s1">` per scene (absolutely stacked, full frame). The timeline is JSON in `frame.json`; Rust **compiles** it to `@keyframes` + `animation` declarations in `<style data-neo="motion">` with every animation `paused`. Time is set from outside by seeking (§11.6) — no script ever lives in the document.

```json
"timeline":{ "fps":30, "scenes":[{"id":"s1","dur_ms":2400,"in":{"kind":"fade","ms":300}},{"id":"s2","dur_ms":3000,"in":{"kind":"slide","dir":"left","ms":400}}],
  "tracks":[{"node":"n4","prop":"transform","keys":[{"t":0,"v":"translateY(var(--space-8))","ease":"--ease-out"},{"t":600,"v":"none"}]},
            {"node":"n4","prop":"opacity","keys":[{"t":0,"v":"0"},{"t":400,"v":"1"}]}],
  "clips":[{"node":"n9","take":"t0051","start_ms":0,"in_ms":500,"dur_ms":2400}],
  "audio":[{"take":"t0060","start_ms":0,"gain_db":-8,"fade_in_ms":200,"fade_out_ms":600,"role":"music"}],
  "captions":[{"t0":200,"t1":2300,"text":"Radio for degens","style":"caption-lower"}] }
```

Animatable props (closed): `transform opacity filter clip-path color background-color object-position letter-spacing` + any `--k-*`. Keyframe `t` is scene-relative. Captions compile to nodes in a generated `div[data-layer="captions"]` styled by kit classes — text is set by the browser engine with brand fonts, never by ffmpeg (Homebrew ffmpeg has no libass) and never by an image model (A12).

## 4. Storage (inside the studio folder, A16)

```
<studio>/ studio.json  takes.jsonl  takes/  fonts/  exports/
  canvas/
    canvas.json            world: frame rects, z-order, board items, lineage-arrow visibility, doc rev
    tokens.json            source of truth · tokens.css generated from it
    kit/ <name>.html  kit.css  kit.json
    frames/<fid>/ frame.html  frame.json  ops.jsonl  pins.jsonl
    briefs/  imports/<host>-<date>/        captured pages (untrusted data)
    cache/  thumbs/ looks/ renders/        disposable; safe to delete
```

Plain text, deterministic serialisation (one element per line, fixed attribute order, 2-space indent) → git-diffable. `dmm` keeps reading `studio.json` / `takes.jsonl` / `takes/` untouched. `ops.jsonl` is append-only; `frame.html` is the materialised head, rewritten atomically (temp + rename) at most every 500 ms and on transaction boundaries from the agent. SQLite holds only indexes (frame list, open pins) — deleting it loses nothing.

## 5. `neo-canvas` — the Rust document model

Tauri-free; used identically by the app and the `neo` CLI. Parsing: **`html5ever`** into our own arena tree (no `kuchikiki`/`scraper` tree — we need stable ids, spans and our serialiser). CSS: **`lightningcss`** to parse, validate, print and minify stylesheets and to resolve `var()` chains for lints *(verify: lightningcss preserves `@layer` order and unknown custom properties verbatim)*. `lol_html` is not used.

```rust
pub struct Doc { pub world: World, pub tokens: Tokens, pub kit: Kit, pub frames: IndexMap<FrameId, Frame> }
pub struct Frame { pub meta: FrameMeta, pub tree: Arena<Node>, pub root: NodeId, pub css: FrameCss /* frame, edit, knobs, motion */,
                   pub rev: Rev, pub log: OpLog, pub pins: Vec<Pin>, pub leases: Vec<Lease> }
pub struct Node { pub id: NodeId, pub tag: Tag, pub attrs: SmallVec<[Attr; 4]>, pub text: Option<String>,
                  pub parent: Option<NodeId>, pub children: Vec<NodeId>, pub role: NodeRole /* derived: Text|Image|Button|Container|Instance|Scene|… */ }
pub enum Scope { Base, Breakpoint(BpId), Placement(PlacementId) }
pub enum Author { User, Agent { task: TaskId, worker: Option<RegionId> }, Jev { utterance: UtteranceId }, System }
```

### 5.1 Op vocabulary

```rust
pub enum Op {
  // structure
  InsertHtml { parent: NodeId, index: usize, html: String },
  RewriteSubtree { root: NodeId, html: String, css: Option<String>, base_rev: Rev },
  Delete { node: NodeId }, Move { node: NodeId, parent: NodeId, index: usize }, Duplicate { node: NodeId },
  Wrap { nodes: Vec<NodeId>, tag: Tag, class: Option<String> }, Unwrap { node: NodeId },
  // content
  SetText { node: NodeId, text: String }, SetAttr { node: NodeId, name: AttrName, value: Option<String> },
  PlaceTake { node: NodeId, take: TakeId, fit: Fit },
  // style (always the `edit` layer)
  SetStyle { node: NodeId, scope: Scope, decls: Vec<(Prop, Option<CssValue>)> },
  BindToken { node: NodeId, scope: Scope, prop: Prop, token: TokenName },
  Compact { root: NodeId },                                  // fold edit → frame
  // tokens, knobs, locks
  SetToken { name: TokenName, value: TokenValue }, KnobDefine { knob: Knob }, KnobSet { knob: KnobId, value: KnobValue }, KnobRemove { knob: KnobId },
  SetLock { node: NodeId, locked: bool },
  // components
  MakeComponent { node: NodeId, name: String }, InsertInstance { parent: NodeId, index: usize, component: String },
  SetSlot { instance: NodeId, slot: String, html: String }, DetachInstance { node: NodeId }, UpdateComponent { name: String, html: String, css: String },
  // frames + world
  CreateFrame { meta: FrameMeta, html: Option<String> }, DeleteFrame { frame: FrameId }, SetFrameMeta { patch: FrameMetaPatch },
  MoveFrame { frame: FrameId, x: f64, y: f64 }, AddBreakpoint { bp: Breakpoint }, AddPlacement { p: Placement }, RemoveScope { scope: Scope },
  BoardPlace { take: TakeId, x: f64, y: f64 }, BoardRemove { item: BoardItemId },
  // pins
  PinAdd { pin: Pin }, PinReply { pin: PinId, text: String }, PinSetStatus { pin: PinId, status: PinStatus },
  // timeline
  SceneAdd { index: usize, scene: Scene }, SceneSet { scene: SceneId, patch: ScenePatch }, SceneRemove { scene: SceneId },
  KeyframesSet { node: NodeId, prop: AnimProp, keys: Vec<Key> }, ClipSet { clip: Clip }, AudioSet { audio: AudioItem }, CaptionsSet { captions: Vec<Caption> },
}
```

Every op is validated, sanitised, and applied with its **inverse computed at apply time** and stored beside it. `apply(op)` then `apply(inverse)` is byte-identical on `frame.html` — a tested invariant.

### 5.2 Transactions

```rust
pub struct Txn { pub id: TxnId, pub author: Author, pub label: String, pub why: Option<String>, pub frame: FrameId,
                 pub ops: Vec<Op>, pub inverses: Vec<Op>, pub footprint: Footprint /* nodes × scopes × props, tokens, knobs */,
                 pub rev_before: Rev, pub rev_after: Rev, pub dropped: Vec<DroppedOp>, pub at: DateTime<Utc> }
```

All-or-nothing. A pointer drag or a knob drag is **one** transaction, committed on release (the surface previews locally meanwhile). Output of a commit is a `Patch` broadcast to every subscriber (§6.6).

### 5.3 Per-author undo / redo

Each author has its own undo stack. `undo(author)` targets that author's newest live transaction **T** even when other authors committed after it:

1. If no later transaction's footprint intersects T's → apply T's inverses as a new transaction (`label: "undo: …"`).
2. **Property rule:** where a later author set the same `(node, scope, prop)` / token / knob, that inverse is skipped — the later value wins — and reported in `dropped`.
3. **Structure rule:** if T is a `RewriteSubtree` / `InsertHtml` / `Delete`, restore the old subtree, then **replay** later foreign ops that fall inside it by `data-n`; ops whose node no longer exists go to `dropped`. If `dropped` is non-empty for a *user's* ops, the UI asks first ("2 of your edits sit on elements this undo removes — undo anyway?").
4. Redo is the mirror image and is cleared by a new transaction from the same author touching the same footprint.

"Revert the agent's pass" = undo every transaction of `Agent{task}` newest-first under the same rules. `Jev` micro-edits undo on the **User** stack (the user said them).

### 5.4 Concurrent edits, leases and rewrite reconciliation

The user is never blocked. An agent worker takes a **lease** on a subtree root (`Lease{root, holder, base_rev}`); leases of different workers may not nest or overlap. When `RewriteSubtree` arrives:

1. **Parse + sanitise** the HTML; reject if the root tag/id changed or a `data-n` from outside the subtree appears.
2. **Id reconciliation.** Kept ids stay. Duplicated ids: first occurrence keeps it, the rest are treated as new. Nodes without an id are matched against old nodes that vanished, by score = tag equal (required) + class Jaccard + text similarity + same parent id + sibling-order distance; a match ≥ 0.75 inherits the old id (so pins, knobs targets, `edit` rules and keyframes survive a rewrite that forgot ids); the rest get fresh ids. The returned `id_map` tells the agent what it got.
3. **Locks.** `locks` nodes are restored byte-identical (content + position among surviving siblings); the rewrite is not rejected for touching them, the change is simply not taken.
4. **Three-way merge** when `base_rev` is stale: user ops committed inside the subtree since `base_rev` are replayed on the new subtree by id. A node whose text the user edited keeps the user's text. A node the user deleted stays deleted. Unreplayable ops → `dropped`, surfaced as a toast with "restore mine".
5. `edit`-layer rules for ids that no longer exist are garbage-collected in the same transaction (kept in the inverse).

### 5.5 Validation and lints

Hard errors reject the transaction: sanitiser violations, unknown token/knob variable, scope that does not exist, override that changes content, region CSS outside its prefix (§8.3). **Lints** never block; they come back on every `canvas_apply` / `canvas_rewrite` / `canvas_look`:

| Code | Source | Fires when |
|---|---|---|
| `off-token` | static | raw colour/length outside tokens |
| `missing-alt` | static | `img` without `alt` (decorative needs `alt=""` + `role=presentation`) |
| `dup-structure` | static | ≥ 3 sibling subtrees with equal structural hash and no component |
| `knob-dead` | static | a `--k-*` no declaration consumes |
| `overflow-x` / `overflow-frame` | render | scroll width > frame width at a scope; node box outside a Graphic/Set/Video frame |
| `text-clip` | render | text node's scroll size > client size |
| `contrast` | render | WCAG ratio < 4.5 (3.0 for ≥ 24 px / 19 px bold) from computed colours; over an image → sampled from the render |
| `tap-target` | render, Web | interactive box < 44×44 CSS px at ≤ 768 |
| `overlap` | render | sibling text boxes intersect |
| `safe-zone` | render, Set/Video | text inside a preset's unsafe band (e.g. story top/bottom 250 px) |
| `engine-drift` | render | WebKit vs Chrome box of any node differs > 2 px (§6.2) |

Render lints come from one `metrics.js` evaluation in the export renderer (box, computed colour, scroll sizes per `data-n`) — JS where a browser engine requires it.

## 6. The webview surface (`ui/canvas/`, TS)

The surface is a view and an input device. It holds no document logic: it sends **intents**, Rust commits ops, patches come back.

1. **World.** One `div.world` with `transform: translate(x,y) scale(z)`; frames are absolutely positioned children. Pan: space-drag / two-finger scroll; zoom: pinch / `⌘`-scroll about the cursor, 2 %–800 %.
2. **Frame isolation = `<iframe>`**, one per live scope, `sandbox="allow-same-origin"` (no `allow-scripts`), `srcdoc` built by Rust with a `<base>` on the Tauri asset protocol scoped to the studio. **Why not a shadow root:** `@media`, `vw/vh`, `position:fixed`, `:root` and `@font-face` must behave exactly as in the exported page, and only a real viewport gives that; the sandbox is a hard script block instead of a sanitiser promise; styles cannot leak either way. Same-origin access lets the parent read boxes and patch the DOM while nothing inside can run. The webview is WebKit and the export renderer is Chrome: the canonical pixels are always the export renderer's — `canvas_look`, exports and lints use it, a **True render** toggle shows its bitmap in place, and `engine-drift` reports disagreement.
3. **Overlay.** Iframes are `pointer-events:none`. One overlay `div` above the world owns all pointer input and draws selection boxes, resize handles, smart guides (edges/centres of siblings + frame, spacing-equal hints), measurements (`⌥`-hover), marquee, pins, agent cursors and region badges in **screen space** (crisp at any zoom).
4. **Hit-testing.** Screen point → world → frame-local → `iframe.contentDocument.elementsFromPoint()` → nearest ancestor with `data-n`. Click selects the top-level child of the current container; double-click descends; `⌘`-click selects the deepest node. Locked nodes are skipped unless chosen in Layers.
5. **Direct manipulation → ops.** Resize writes `width/height/aspect-ratio` snapped to tokens; dragging a flow child reorders (`Move`) with an insertion indicator; dragging an absolutely positioned node writes `inset` values; `⌥`-drag = `Duplicate`. Everything is `SetStyle`/`Move` in the active scope.
6. **Patches.**

```ts
type Patch = { frame: FrameId; rev: number; txn: TxnId; author: Author; changes: Change[]; lints?: Lint[] };
type Change = { k:'replace'; n:NodeId; html:string } | { k:'insert'; parent:NodeId; index:number; html:string } | { k:'remove'; n:NodeId }
            | { k:'move'; n:NodeId; parent:NodeId; index:number } | { k:'text'; n:NodeId; text:string } | { k:'attr'; n:NodeId; name:string; value:string|null }
            | { k:'css'; layer:'frame'|'edit'|'knobs'|'motion'|'tokens'|'kit'; text:string } | { k:'meta'; patch:Partial<FrameMeta> };
```

   Delivered on one Tauri `Channel<Patch>` per open studio, coalesced to ≤ 60 Hz; applied by `data-n` lookup (a `Map<NodeId, Element>` per iframe). A `rev` gap → the surface requests a full `srcdoc`. Agent rewrites stream: Rust parses the tool-call argument incrementally and emits `insert` changes as subtrees close, so the design visibly builds.
7. **Text editing.** Double-click a text node → that iframe gets pointer events, the node gets `contenteditable="plaintext-only"`; `Enter` = line break in multi-line roles, else commit; `Esc` cancels; blur commits one `SetText`. Rich inline marks (`strong`, `em`, `a`) via `⌘B/⌘I/⌘K` become `RewriteSubtree` of that node.
8. **Virtualisation.** Live iframes only for scopes intersecting the viewport at zoom ≥ 15 %, capped at **12**; everything else is a cached thumbnail (`cache/thumbs/<fid>@<scope>@<rev>.webp`, produced by the export renderer at idle). Boards virtualise take tiles the same way.

## 7. Agent tools (`neo-canvas-agent`)

Always all present in a `design` task's tool list (the list never varies mid-task, 03). Every call takes `why`.

| Tool | Args | Returns |
|---|---|---|
| `canvas_outline` | `scope?: world \| frame \| node`, `depth?`, `at?: scope id` | outline text (below) |
| `canvas_frame_create` | `kind`, `name`, `size \| preset`, `near?: frame` | `frame`, root id |
| `canvas_rewrite` | `frame`, `root`, `html`, `css?` (replaces this region's `frame`-layer rules), `base_rev`, `pass` | `txn`, `rev`, `id_map`, `reconciled[]`, `dropped[]`, `lints[]` |
| `canvas_apply` | `frame`, `ops[]` | `txn`, `rev`, `lints[]` |
| `canvas_look` | `frame`, `node?`, `at?`, `t_ms?`, `scale?` (default fits 1024 px), `annotate?` (draw `data-n` labels) | image part + render lints |
| `canvas_variants` | `frame`, `n` (2–4), `axis: layout \| palette \| copy \| imagery \| type`, `brief?` | new frame ids laid out beside the source, `lineage.variant_of` set |
| `canvas_place_take` | `frame`, `node \| board`, `take`, `fit` | `txn` — all `media_*` tools (07) compose with this |
| `canvas_pin_list` / `canvas_pin_reply` / `canvas_pin_resolve` | `frame?`, `status?` / `pin`, `text` / `pin`, `txn` | pins with node, scope, thread |
| `canvas_knobs_set` | `frame`, `knobs[]` (3–8; replaces agent-authored knobs, keeps user-pinned ones) | accepted knobs with clamped ranges, rejects with reasons |
| `canvas_tokens_get` / `canvas_tokens_propose` | — / `patch`, `reason` | tokens / a proposal card the user accepts (studio-wide changes are never silent) |
| `canvas_import_url` | `url`, `mode: frame \| tokens \| both`, `breakpoint?` | new Web frame and/or token proposal; gated (§12) |
| `canvas_export` | `target: frame \| set \| selection`, `format`, `options`, `dest?` | file list + pre-export check report; gated when `dest` is outside the studio |
| `canvas_timeline_outline` · `canvas_scene_add` · `canvas_scene_set` · `canvas_keyframes_set` · `canvas_captions_set` · `canvas_audio_set` · `canvas_preview` | Video frames; `canvas_preview(frame, t_ms)` = `canvas_look` at a time | timeline text / `txn` / image |

**Outline format** (≤ 6k tokens; text truncated at 60 chars; deeper levels elided with counts):

```
frame f3 "Launch ad" set 1080x1080 rev 412 at:master scopes:[1x1 9x16 16x9] lints:1
n1 body.ad grid rows[auto 1fr auto] pad=--k-pad bg=colour-bg
  n2 img.logo take:t0042 "Degen Radio logo" 160x48
  n3 div.copy col gap=space-4
    n4 h1.headline "Radio for degens" --k-headline/800 colour-text   pin:p2 open "feels weak"
    n5 p.sub "Live charts, zero ads." text-xl colour-muted           lint:contrast 3.9
  n6 a.cta «button v2» "Listen now" bg=colour-accent r=radius-full    [locked]
knobs: k1 "Headline size" --k-headline text-5xl…text-9xl =text-7xl · k2 "Padding" --k-pad space-4…space-16 =space-8
```

Text that came from `canvas_import_url` is wrapped as `untrusted"…"` — data, never instructions (A9).

## 8. How the agent designs

### 8.1 Layered passes (each pass = one or more transactions labelled with the pass)

1. **Brief** — the 07 brief (audience, message, mood, brand kit, `soul.md` voice, outputs), saved to `canvas/briefs/`.
2. **Skeleton** — frames + regions as real HTML structure with token-only greyscale styling; for a Set, the master only.
3. **Content** — real copy; no lorem; alt text written here.
4. **Imagery** — `media_*` shoot-outs land on a Board beside the frame; winner placed with `canvas_place_take`; existing studio takes reused first.
5. **Refine** — type scale, rhythm, contrast, alignment → `canvas_look` → critique → fix. **Max 3 rounds**, then stop and show the user.
6. **Knobs** — `canvas_knobs_set` for what this design's open questions are.
7. **Fan-out** — breakpoints (Web), placements (Set), scenes (Video); then `canvas_look` every scope once.

**Critique rubric** (scored 0–3 each, written to the trace with the look image; a round is required while any item < 2): hierarchy (one focal point) · legibility at thumbnail (look at 25 %) · one clear CTA · breathing room / rhythm on the spacing scale · brand fit (tokens, voice) · imagery quality + crop · zero render lints · scope sanity (every breakpoint/placement).

### 8.2 Presence

An **agent cursor** in the task's colour sits on the node being changed; leased regions show a soft outline + `RegionBadge` with the worker label and pass. The Mind pane shows pass, transactions, critique text, before/after looks.

### 8.3 Parallel region workers

The one place Sol calls run concurrently (A10). A coordinator does the skeleton and marks regions (`frame.json.regions`). Up to **4** workers; each holds a lease on its region root and may only (a) `canvas_rewrite`/`canvas_apply` inside it, (b) write `frame`-layer rules whose selectors start with its prefix (`.hero-…`) — enforced by the validator — and (c) read tokens and kit. Workers cannot write tokens or kit; they send `canvas_tokens_propose` to the coordinator, which decides once for all. Merge is trivial by construction (disjoint subtrees, disjoint selectors); the coordinator then runs a **unify pass** on the whole frame: inter-region rhythm, duplicate patterns → kit components (`dup-structure`), one refine round. `canvas_variants` uses the same machinery, one worker per variant frame. Each worker's spend counts against the task cap.

## 9. Pins, knobs, micro-edits (A14)

### 9.1 Pins

```rust
pub struct Pin { pub id: PinId, pub node: NodeId, pub scope: Scope, pub anchor: (f32, f32) /* 0–1 in the node box */, pub text: String,
                 pub source: Source /* voice|typed */, pub status: PinStatus /* Open|Working|Resolved|Dismissed */, pub task: Option<TaskId>,
                 pub thread: Vec<PinMsg>, pub created_rev: Rev, pub resolved_txn: Option<TxnId>, pub orphaned: bool }
```

Create: `C` + click, or select a node and say "this feels weak". A pin becomes a queued `design` task whose goal is the pin text, whose **lease is the pinned node's subtree**, and whose context is that subtree's outline + a cropped look. The agent replies in the thread and resolves with the fixing `txn`; resolved pins collapse into History. If the node is deleted the pin moves to the nearest surviving ancestor with `orphaned: true`. Several open pins on one frame run as region workers when their subtrees are disjoint, else sequentially.

### 9.2 Knobs

```rust
pub struct Knob { pub id: KnobId, pub label: String, pub var: String /* --k-… */, pub kind: KnobKind, pub value: KnobValue, pub default: KnobValue,
                  pub level: KnobLevel /* Frame|Set|Studio */, pub targets: Vec<NodeId>, pub aliases: Vec<String>, pub pinned_by_user: bool, pub author: Author }
pub enum KnobKind { TokenScale { scale: ScaleName, from: TokenName, to: TokenName }, Range { min: f64, max: f64, step: f64, unit: Unit },
                    Choice { options: Vec<(String, CssValue)> }, Toggle { on: CssValue, off: CssValue }, ColourRamp { tokens: Vec<TokenName> } }
```

Generation: Sol emits 3–8 knobs naming this design's real degrees of freedom ("hero spacing", "headline size", "accent warmth", "image crop", "card density"). Rust accepts a knob only if its variable is consumed by ≥ 1 declaration; it renders both extremes and **clamps the range to the lint-clean interval**. Dragging: the surface calls `style.setProperty` on the iframe root per pointer move — **no Rust round trip, no model call** — and commits one `KnobSet` on release. Hovering a knob highlights `targets`. Voice: knob labels + `aliases` are offered in the micro-edit request; "a bit more hero spacing" → `knob_up` on that knob.

### 9.3 Jev micro-edits — the single TypeSafe request

When Design mode is focused and a frame is in view, every utterance/message fires **two Jev requests in parallel**: intake (03) and the micro-edit request. The micro-edit result is applied only if intake says `new_task|amend` with route `design`; otherwise it is discarded.

**State** (structured JSON via `jev-nav::wire`):

```json
{ "utterance":"make the headline a lot bigger",
  "frame":{"id":"f3","kind":"set","at":"master","size":[1080,1080]},
  "selection":["n4"], "hover":"n2", "last_edit":{"target":"n4","operation":"TEXT_BIGGER","amount":"SOME"},
  "nodes":[{"id":"n4","role":"text","tag":"h1","label":"Radio for degens","parent":"n3","order":0,"flow":"col","selected":true,
            "now":{"font-size":"text-7xl","weight":"800","colour":"colour-text"},"ops":["TEXT_BIGGER","TEXT_SMALLER","WEIGHT_UP","…"]}, "… ≤ 120 nodes: selection, its siblings/ancestors, then in-view by area"],
  "knobs":[{"id":"k1","label":"Headline size","aliases":["title size"],"at":"3/5"}],
  "colours":["text","muted","accent","accent-2","bg","surface"], "recent_edits":["… last 6"] }
```

**Heads** (answered independently; the executor reads `operation`, then only the heads that operation consumes):

| Head | Type | Options |
|---|---|---|
| `operation` | choice | the ops valid for the offered nodes, + `NEEDS_SOL`, `NOT_AN_EDIT` |
| `target` | choice | node ids + `SELECTION` + `FRAME` |
| `second_target` | choice | node ids — consumed by `SWAP`, `ALIGN_TO`, `MATCH_SIZE` only |
| `amount` | choice | `A_TOUCH` · `SOME` · `A_LOT` · `ALL_THE_WAY` |
| `knob` | choice | knob ids — consumed by `KNOB_UP/DOWN/RESET` only |
| `colour` | choice | token colour names — consumed by `COLOUR_SET`, `BG_SET` only |
| `all_scopes` | yes_no | "Does the user mean every size/breakpoint, not only the one in view?" → `Scope::Base` vs the active scope |

**Closed op vocabulary:** `TEXT_BIGGER TEXT_SMALLER WEIGHT_UP WEIGHT_DOWN TRACKING_TIGHTER TRACKING_LOOSER LEADING_TIGHTER LEADING_LOOSER TEXT_ALIGN_LEFT TEXT_ALIGN_CENTER TEXT_ALIGN_RIGHT CASE_UPPER CASE_NORMAL SET_TEXT · SCALE_UP SCALE_DOWN FILL_WIDTH HUG MATCH_SIZE · PAD_MORE PAD_LESS GAP_MORE GAP_LESS SPACE_ABOVE_MORE SPACE_ABOVE_LESS SPACE_BELOW_MORE SPACE_BELOW_LESS · MOVE_LEFT MOVE_RIGHT MOVE_UP MOVE_DOWN MOVE_EARLIER MOVE_LATER SWAP BRING_FORWARD SEND_BACK · ALIGN_LEFT ALIGN_CENTER ALIGN_RIGHT ALIGN_TOP ALIGN_MIDDLE ALIGN_BOTTOM ALIGN_TO DISTRIBUTE · COLOUR_SET BG_SET COLOUR_NEXT LIGHTER DARKER CONTRAST_UP · RADIUS_MORE RADIUS_LESS SHADOW_MORE SHADOW_LESS OPACITY_MORE OPACITY_LESS · CROP_TIGHTER CROP_LOOSER FIT_COVER FIT_CONTAIN FOCAL_LEFT FOCAL_RIGHT FOCAL_UP FOCAL_DOWN · HIDE SHOW DUPLICATE DELETE LOCK UNLOCK SELECT · KNOB_UP KNOB_DOWN KNOB_RESET · UNDO REDO UNDO_AGENT · NEEDS_SOL NOT_AN_EDIT`.

`MOVE_*` is resolved by Rust from layout, not by Jev: in a flex/grid flow along that axis → reorder; across the axis → `align-self`/`justify-self` step; absolutely positioned → `inset` step. `SET_TEXT` mirrors the navigator's `TYPE_TEXT`: the **text helper** returns `{"text": …}` only when the utterance states the literal words, else `null` → `NEEDS_SOL`.

**Amount → token-scale steps** (values always land on a token; never 37.5 px). Default when nothing is said = `SOME`.

| Scale | `A_TOUCH` | `SOME` | `A_LOT` | `ALL_THE_WAY` |
|---|---|---|---|---|
| type ramp, weight, radius, shadow | 1 stop | 1 stop | 2 stops | end of ramp |
| spacing (pad, gap, space, inset) | 1 stop | 2 stops | 4 stops | scale end / 0 |
| size (`SCALE_*`), crop | 5 % | 10 % | 25 % | fit container |
| colour lightness (ramp 50–950), opacity | 1 stop | 2 stops | 3 stops | ramp end |
| knob | 1 step | 2 steps | 4 steps | min/max |

**Thresholds.** Apply when `P(operation) ≥ 0.60` and `P(target) ≥ 0.55`. Operation clear but top two targets within 0.15 → highlight both, ask "which one?" with chips (no Sol). `P(NEEDS_SOL) ≥ 0.40`, or no operation ≥ 0.60 → hand the utterance to Sol as a `design` task with the selection as context. `DELETE` needs `≥ 0.80` (it is undoable, so no confirm card). `NOT_AN_EDIT ≥ 0.5` → drop, intake decides alone. Answer validation is the navigator's (choice ∈ offered, probabilities sum ≈ 1, argmax == choice) — otherwise nothing is applied. Jev outage → micro-edits fail to Sol, never to a guess.

**`NEEDS_SOL` by definition:** new content or structure, rewriting copy, mood/style changes, new imagery, new placements or breakpoints, anything referring to a frame not in view.

**Latency budget** (end of speech → pixels): STT 300–500 ms · intake ∥ micro-edit Jev ≤ 180 ms · resolve + commit ≤ 5 ms · patch + paint ≤ 16 ms → **≤ 0.7 s spoken, ≤ 0.25 s typed**. Repeats ("bigger… bigger") reuse `last_edit` and skip nothing but still step again.

## 10. Tokens, brand kit, design-system ingestion

`tokens.json` is seeded from the studio brand kit (`studio.json`: palette, fonts, logo, voice): colour roles (`bg surface text muted accent accent-2` + contrast partners) and ramps 50–950; type families + ramp `text-xs…text-9xl` (size / line-height / tracking); weights; `space-0…space-32` on a 4 px base; radius; shadow; motion; breakpoints. `SetToken` regenerates `tokens.css`; every frame of every kind updates. A **starter kit** (nav, hero variants, feature grid, logo row, testimonial, pricing, FAQ, CTA, footer, ad layouts, caption styles) is *generated* per studio from the tokens by a Sol pass — never hard-coded templates.

**`canvas_import_url`.** The URL loads in a bot-owned tab of Stark's Chrome; `capture.js` (ours, injected) returns in one evaluation: the visible DOM reduced to the allowed subset, computed values of a fixed property whitelist per element, image URLs, `@font-face` sources. Rust then: downloads images → imports them as takes (`source: import`, origin recorded); sanitises; rewrites computed styles into classes; **clusters** colours (area-weighted → roles + ramps), font sizes (→ ramp), paddings/gaps (→ base unit), radii; detects repeated sibling structures → component candidates. Result: an editable Web frame and/or a **token proposal card** (accept all / pick). Webfont files are downloaded only with the user's tick (licence is theirs to judge); otherwise the nearest installed/studio family is mapped and the original name recorded. A design system can also be ingested from a folder of HTML/CSS picked with the OS open panel (P3) through the same pipeline.

## 11. Export (`neo-canvas::render`, feature `render`)

**Export renderer.** A dedicated **headless Chrome** process (same binary as Stark's Chrome, own throw-away profile, debugging pipe) driven by the `jev-nav` CDP client — never the navigator's tabs, so canvas work and desktop work do not collide (A10). One page per job: write `cache/renders/<job>.html` (frame + resolved scope), `Page.navigate` to it, wait for `document.fonts.ready` + image decode, run.

1. **PNG / JPG / WebP.** `Emulation.setDeviceMetricsOverride{width,height,deviceScaleFactor: 1|2|3, mobile:false}` → `Page.captureScreenshot{format:"png", clip:{x,y,width,height,scale:1}, captureBeyondViewport:true}`; transparent background via `Emulation.setDefaultBackgroundColorOverride{a:0}`. Always captured as PNG; JPG and **lossy WebP are encoded in Rust** (`image` for JPG, the `webp` crate for lossy — the `image` crate's WebP is lossless only). Web frames capture full content height.
2. **SVG assets via `resvg`.** Vector takes, logos, lockups, favicon/app-icon sets rasterise through `resvg` at exact sizes. A frame exports as SVG only when it is pure inline SVG; otherwise the option is disabled with the reason — no `foreignObject` tricks.
3. **PDF.** Inject `@page{size:<w>px <h>px;margin:0}` → `Page.printToPDF{printBackground:true, preferCSSPageSize:true}`. Multi-frame PDFs (slides, a Set): one print document with one page per frame *(verify mixed page sizes; fallback: per-frame PDFs merged with `lopdf`)*.
4. **HTML + Tailwind.** Semantic HTML with `data-n`/`data-neo-*` stripped; `edit` compacted into `frame`; token-valued declarations mapped to utilities by a Rust table, the rest kept in a small `styles.css`; tokens emitted as `tokens.css` + a Tailwind theme block *(verify the Tailwind v4 CSS-first `@theme` shape)*. Images → WebP + `srcset`; `<title>`/meta/OG filled (the OG image is a Graphic frame in the same studio). We emit source only and never run a Node toolchain (P3).
5. **React + TS, DRY.** Kit components → one `.tsx` each, slots → typed props, `--c-*` → variant props. Remaining repeats: subtrees with equal structural hash (tags + classes, ignoring text/src), ≥ 2 occurrences, ≥ 3 nodes → extracted component; ≥ 3 consecutive siblings → a typed data array + `.map`. Regions → section components; breakpoints stay media queries/utilities. Output: `components/`, `sections/`, `Page.tsx`, `data.ts`, `tokens.css`, `assets/`.
6. **MP4 by deterministic frame stepping.** All animations are compiled `paused`. For frame *i*: `Runtime.evaluate` → `document.getAnimations().forEach(a => a.currentTime = t)` with `t = i·1000/fps` (our expression; nothing lives in the document) → `Page.captureScreenshot` → bytes piped to `ffmpeg -f image2pipe -framerate <fps> -i - -c:v libx264 -pix_fmt yuv420p -crf 16`. Wall-clock never matters, so output is reproducible. Long videos split into frame ranges across up to 4 pages → segments → concat. `Emulation.setVirtualTimePolicy` and `HeadlessExperimental.beginFrame` are **not** used: seeking paused animations needs neither *(verify in M0 that a screenshot after a seek always reflects it; if not, await one `requestAnimationFrame` in the same evaluate)*. **Clips:** `video` nodes render as transparent holes; HTML is captured with alpha; ffmpeg overlays the PNG stream above the clips placed at each node's box (v1 rule: clips sit below all HTML in their scene; lint otherwise). **Audio:** ffmpeg `adelay`/`volume`/`afade`/`amix`; voice-over lines come from `neo-voice` TTS as audio takes; music ducks −10 dB under `role: voice`. **Captions** are already pixels in the HTML layer.
7. **GIF** = the same frame stream → ffmpeg `palettegen` + `paletteuse`, ≤ 15 fps, ≤ 720 px default. **Animated SVG** = pure-SVG Video frames only: the inline SVG + compiled `@keyframes` with real delays, un-paused.
8. **Handoff bundle** (`exports/<name>-handoff/`): `README.md` (what this is, how to run), `BRIEF.md`, `src/` (5) or `html/` (4), `tokens.json` + `tokens.css`, `assets/` (WebP + originals), `renders/` (PNG per scope), `outline.txt`, `pins-open.md`, `checks.json`, `lineage.json` (takes + prompts + models). Shaped for a coding agent; nothing is pushed anywhere.

**`WKWebView` fallback** (Chrome absent): an off-screen `WKWebView` in `src-tauri` implements the same `Renderer` trait — `takeSnapshot` for raster, `createPDF` for PDF, the same seek expression via `evaluateJavaScript` for video *(verify snapshot scale control above the display's factor)*. Marked **degraded** in the export sheet: no `engine-drift` lint, slower video, not available in the headless `neo` CLI.

**Pre-export checks** (report returned by `canvas_export`; errors block, warnings need one click): zero `overflow-x` at every scope · `contrast` AA · every `img` has alt · `text-clip`/`overflow-frame` none · fonts all resolved from the studio · links non-empty · image weight budget (Web: ≤ 300 KB per image, ≤ 1.5 MB page) · focus order follows DOM · `tap-target` · Set: every placement looked at since its last change · Video: audio ≤ duration, captions inside safe zone, fps/size match the preset.

## 12. Takes, lineage, Board — Studio parity

| 07 Studio feature | On the canvas |
|---|---|
| takes grid, newest first, live jobs | `TakesShelf` (left rail) + Board frames; in-flight jobs are placeholder tiles (Quiver SVG drafts stream in; fal shows queue position) |
| star / note / delete / drag out | tile context menu; drag a tile to Finder or into any frame (`PlaceTake`) |
| lineage strip, "branch from here" | select a take node → lineage strip in Inspector; **lineage arrows** between tiles on a Board (toggle) |
| viewer: zoom/pan, checkerboard, scrubber, A/B wipe | canvas zoom is the viewer; `⇧A` wipes between two selected takes; video tiles scrub |
| local edits without an LLM | crop / focal point / fit / flip / background / recolour SVG fills from tokens = ops on the node; "save as take" renders the node to a new take |
| instruction bar | the composer + voice with the selection as context → `media_edit` / `media_vector_edit` |
| ads & lockups forms | **Set** frames from kit ad layouts; lockups are Graphic frames; re-running = editing the master |
| brand kit panel | `TokensPanel` → Brand tab (writes `studio.json` + `tokens.json`) |
| "that one" context | intake facts: `studio`, `frame_in_view`, `selected_nodes`, `selected_takes`, `last_created_takes` |

With `neo-media` disabled the canvas works fully; takes tools are absent and the shelf shows the "Enable media" card (07).

## 13. Video on the canvas

Selecting a Video frame opens the **timeline dock**: scene blocks (drag to reorder, edge-drag duration, transition chips: cut · fade · slide · scale · mask-wipe) · one property track per animated `(node, prop)` with keyframes + easing · clip track · audio track (waveform from ffmpeg-decoded peaks) · caption track · playhead. Playback in the canvas = the surface seeking the iframe's animations on `requestAnimationFrame` through the same-origin handle (the document still holds no script); `video` clips and audio play natively in sync. Templates ("kinetic headline", "product spin + CTA", "before/after") are kit scenes + timelines. `canvas_preview(t)` gives Sol a frame for critique; the refine rubric adds *pacing* and *first-second hook*. Anything beyond motion-graphics scale — multi-clip edits, reframing, colour — goes to **starflux** later as the `neo-video` pack; the Video frame stays the storyboard and receives the result back as a take.

## 14. Gating and cost

Canvas ops are local, free and undoable → **no Jev risk gate, no confirm card**. Gates stay where they already are: paid `media_*` calls (K5: $0.25 per call before a confirm), `canvas_export` with `dest` outside the studio (pre-gate, OS save panel), anything that publishes (outward → confirm), `canvas_import_url` (navigates a bot tab; deny-listed origins refuse; login walls → `BLOCKED`). Sol passes and region workers count against the $1.00 task cap; at the cap the task stops **at a pass boundary** with the frame valid and asks. Imported content is untrusted data everywhere it appears. With the screen locked, canvas tasks pause like all tasks (P7).

## 15. Design mode UI

```
┌ listening bar (shared, always) ─────────────────────────────────────────────────────┐
│ Layers · Frames · Tokens │                H Y P E R C A N V A S           │ Inspector │
│ ──────────────────────── │   frames · boards · pins · agent cursors       │ Knobs     │
│ Takes shelf              │   scope tabs on the selected frame              │ Style·Layout·Take·Export │
├──────────────────────────┴───────────── timeline dock (Video frames) ─────┴───────────┤
│ conversation strip: last turns · composer · current pass + Steer ticker · History     │
└───────────────────────────────────────────────────────────────────────────────────────┘
```

Knobs sit at the **top** of the right rail — they are the primary refinement control, the inspector is the precise one. History lists transactions by author with thumbnails; scrub to any rev; "revert the agent's pass". Design mode can pop out as its own window; both share one store.

**Components (DRY; reused from 04: `ListeningBar`, `ConversationThread` (strip variant), `Composer`, `TraceTimeline`, `ConfirmPrompt`, `AskPrompt`):** `CanvasWorld`, `FrameView` (one component for all kinds), `FrameChrome` (title, scope tabs, lint badge), `ThumbView`, `Overlay` → `SelectionBox`, `Handles`, `SmartGuides`, `Marquee`, `Measure`, `PinMarker`, `AgentCursor`, `RegionBadge`; `LayersTree`, `FramesList`, `TokensPanel` + `TokenRow`, `TakesShelf` + `TakeTile`, `Inspector` + `PropRow` (one row component for every property, token-aware), `KnobsPanel` + `KnobControl`, `PinThread`, `LintList`, `HistoryPanel`, `ExportSheet` + `CheckReport`, `TimelineDock` → `SceneStrip`, `Track`, `KeyframeDot`, `Playhead`, `CommandPalette`.

**Hooks:** `useCanvasDoc()` (patch channel → Zustand store; the only subscriber) · `useFrame(fid)` · `useSelection()` · `useViewport()` · `useIntent()` (the single way to send an intent → op) · `useHitTest()` · `useDragSession()` (shared by move/resize/knob/keyframe drags: preview locally, commit once) · `useKnobs(fid)` · `usePins(fid)` · `useLints(fid)` · `useTimeline(fid)` · `useAgentPresence()` · `useHistory(author?)` · `useExport()`. Types come from Rust via `tauri-specta` (A15); no hand-written mirror types.

**Shortcuts.** `V` select · `F` frame · `T` text · `R` box · `I` image/take · `C` pin · `K` focus knobs · `H`/space pan · `⌘K` palette · `⌘Z` / `⇧⌘Z` my undo/redo · `⌥⌘Z` undo the agent's last transaction · `⌘G` / `⇧⌘G` wrap/unwrap · `⌘D` duplicate · `⌥`-drag duplicate · `⇧` constrain · arrows nudge one spacing stop (`⇧` = 4) · `⌘]` / `⌘[` forward/back · `⌘L` lock · `⌘⇧H` hide · `⌘1` fit all · `⌘2` fit selection · `⌘0` 100 % · `[` / `]` previous/next scope · `⇧T` True render · `⇧A` A/B wipe · `⌘E` export · `⌘⏎` send selection to the agent · Video: `space` play, `,` `.` frame step, `S` split scene, `⇧K` add keyframe.

## 16. Performance budgets

| What | Budget |
|---|---|
| op commit (validate + apply + inverse + patch) on a 2,000-node frame | ≤ 5 ms p95 |
| parse + sanitise + reconcile a 500-node rewrite | ≤ 25 ms |
| patch → painted | ≤ 16 ms; pan/zoom 60 fps with 12 live iframes + 200 thumbnails |
| knob / drag preview | 0 Rust round trips per pointer move |
| micro-edit, end of speech → pixels | ≤ 0.7 s (typed ≤ 0.25 s) |
| `canvas_look` (warm renderer, 1080² @1×) | ≤ 400 ms; thumbnails ≤ 150 ms at idle priority |
| PNG export 1080² @2× | ≤ 1 s · a 6-placement Set ≤ 5 s |
| MP4 1080p30 | ≤ 2× real time on Apple Silicon with 4 pages |
| studio open, 50 frames | ≤ 500 ms to first paint (thumbnails first) |
| outline | ≤ 6k tokens, ≤ 10 ms |

## 17. Test plan (Rust only)

- **Op-log property tests** (`proptest`): `apply∘inverse = identity` on bytes; `serialise∘parse = identity`; replaying `ops.jsonl` from empty reproduces `frame.html`; per-author undo with disjoint footprints commutes; property/structure conflict rules produce the documented `dropped`; ids are never reused.
- **Reconciliation tests:** rewrites that drop ids, duplicate ids, reorder, touch locked nodes, race with user text edits — asserted `id_map`, surviving pins/knobs/`edit` rules.
- **Sanitiser corpus:** script/handler/URL/CSS injection fixtures, including captured hostile pages; output must contain nothing executable and no non-studio URL.
- **Golden-render tests** (`--features render-tests`, needs Chrome): fixture frames with bundled fonts → PNG at 1×/2×, perceptual diff ≤ tolerance; PDF page count/size; video: frame *i* is identical across two runs and across 1-page vs 4-page splits; ffmpeg mux duration/fps asserted with `ffprobe`.
- **Export fixtures** (`insta` snapshots): HTML+Tailwind, React+TS (componentisation of a pricing grid, a feature list → `.map`), handoff bundle tree.
- **Lint fixtures:** each code has a firing and a non-firing frame.
- **Micro-edit policy:** offline tests for request building, answer validation, amount→step tables, `MOVE_*` resolution; labelled utterance set replayed by `neo judge eval` for thresholds.
- **Surface harness:** the `ui/canvas` bundle is loaded in headless Chrome and driven from Rust over CDP (click → intent → patch → DOM assert) — still no non-Rust test runner.
- `neo canvas …` CLI parity: `outline · apply · rewrite · look · export · import-url` run headless on a studio folder.

## 18. Milestones and acceptance

| M | Scope | Done when |
|---|---|---|
| **M0** spike | frame HTML → headless Chrome → `captureScreenshot` with clip + DSF; seek-and-capture 90 frames twice; same frame in a sandboxed iframe inside the Tauri webview; WebKit-vs-Chrome box diff on 5 fixture frames | bytes identical across runs; drift ≤ 2 px on all fixtures or the divergent CSS is added to the lint list; same-origin sandboxed iframe patching confirmed in WKWebView |
| **M7** Hypercanvas I | `neo-canvas` (parse, ops, transactions, per-author undo, leases, reconciliation, lints, storage, tokens, kit); surface (world, iframes, overlay, text edit, virtualisation, patches); Graphic / Set / Board; takes as nodes + lineage; tools outline/rewrite/apply/look/variants/place_take/knobs/pins/export; passes + rubric; pins, knobs, micro-edits; PNG/JPG/WebP/PDF/SVG-asset export; §12 parity | by voice only: "make a square ad for Degen Radio's launch, three options" → three Graphic frames build on the canvas; "the second one — bigger headline, move the logo left" → each ≤ 0.7 s, zero Sol calls; drag a generated knob → instant; pin "this feels weak" → scoped fix, pin resolved; "now all the placements" → a Set exports every size with a clean check report; `⌥⌘Z` reverts the agent's pass and keeps the user's tweaks |
| **M8** Hypercanvas II | Web frames: breakpoints + scope overrides, component kit generation, region workers + unify pass, render lints at every breakpoint, HTML+Tailwind, React+TS, `canvas_import_url` (frame + tokens), handoff bundle, History scrubber | "redesign my landing page at <url>" → imported frame + token proposal → 4-region parallel build → zero `overflow-x`/`contrast` errors at 4 breakpoints → React export builds in a stock Vite+TS project unchanged → handoff bundle complete |
| **M12** Video | Video frames, timeline dock, compile-to-keyframes, preview seeking, clips, captions, audio, MP4/GIF/animated SVG, `canvas_*` timeline tools | "make a 10-second vertical launch video from the ad" → scenes + captions + music → MP4 1080×1920 30 fps, reproducible frames, A/V in sync (± 1 frame), render ≤ 2× real time |

## 19. Risks

| Risk | Mitigation |
|---|---|
| WebKit (canvas) vs Chrome (export) render differently | restricted CSS subset, studio-bundled fonts only, `engine-drift` lint, True render toggle, canonical pixels always Chrome; measured in M0 |
| Agent rewrites lose ids → pins/knobs/overrides detach | reconciliation (§5.4), `id_map` feedback, rewrites scoped to the smallest subtree, property tests |
| A rewrite clobbers what the user just did | never block the user; three-way merge; locks; `dropped` always surfaced with one-click restore |
| Per-author undo surprises | footprint rules are few and shown in History; destructive undo of user ops asks first |
| CDP frame stepping too slow or not paint-accurate | 4-page parallel ranges; M0 proves seek→screenshot; JPEG q95 pipe when no alpha |
| Headless Chrome absent / updated under us | `WKWebView` fallback behind one `Renderer` trait; `neo doctor` reports renderer + version |
| Tailwind/React export quality on arbitrary CSS | only token-valued declarations map to utilities, the rest stays plain CSS; fixtures; export never runs a toolchain |
| Imported pages carry hostile markup or instructions | single sanitiser on every entry path; `untrusted"…"` in outlines; no network at render time |
| Region workers multiply spend | cap 4, task cap enforced at pass boundaries, estimate shown before fan-out |
| Micro-edit picks the wrong node | selection-first State, ambiguity chips instead of guessing, everything one `⌘Z` away |
| Webfont licences on import | download is opt-in per import; default maps to studio fonts |
