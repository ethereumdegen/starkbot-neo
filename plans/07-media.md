# 07 — Media engine (`neo-media` + the `degen-media-maker` library)

Constitution: P2 (media must be *very good*), K1, K2, K4, K5, A10, A12, A13, A16, M6.

## 1. Scope

This doc specifies the **engine**: generating, editing, vectorising, animating, finishing and exporting media; the takes ledger; the backends; the tools Sol gets; the quality pipeline; spend control; the embedded `neo-media` pack and its enablement flow.

It does **not** specify an interactive surface. Everything a person sees or touches — takes on a Board, lineage, A/B compare, Sets, the brand kit panel — belongs to the Hypercanvas (11). §14 is the parity checklist the canvas must meet; nothing else here is UI.

Hard rules: **no OpenAI image generation** (K4) — the OpenAI key does inference and speech only. **No OpenRouter** (K2). Rust only (A1). All text in a deliverable is set by our renderer, never by an image model (A12).

Ownership: `degen-media-maker` (lib) owns studios, the ledger, backends, the router, local pixel/SVG work and the compositor. `neo-media` owns tools, gates, spend, Keychain credentials, events and the pack. `neo-canvas` owns frames; it consumes takes and `AppEvent::Media*`.

## 2. `degen-media-maker`: lib + bin split

Today the crate is bin-only: `src/main.rs` + 13 modules (~3.9k lines), blocking `reqwest`, `thread::scope` for shoot-outs, keys read from `.env` files. The split keeps `dmm` behaving identically and makes the same code embeddable.

**Decisions.** One package, `[lib] degen_media_maker` + `[[bin]] dmm`. The lib is **async** (`tokio`, async `reqwest`); pixel work runs in `spawn_blocking`; `dmm` builds a runtime in `main`. The lib never prints, never reads the environment and never exits the process — it returns typed errors and emits `Progress`. Cargo features: `cli` (clap, stdout reporting, skill installer), `env-keys` (the `.env` / `~/.degen-media-maker/keys.json` / shell lookup in today's `keys.rs`), `preview` (the axum board on 127.0.0.1). `neo-media` enables none of them. The current `live::Job` is renamed `LiveJob` because `Job` becomes the backend request type.

```
src/lib.rs            re-exports; MediaError
src/studio/           mod.rs (Studio, StudioConfig, Brand) · ledger.rs (Take, Kind, append, fold, lock) · ids.rs (reserve_id) · resolve.rs
src/backend/          mod.rs (MediaBackend, Capabilities, Job, Artifact, Progress, Usd) · router.rs · fal.rs · quiver.rs · mock.rs (test-util feature)
src/catalog.rs        aliases → (backend, model id, role), per-model body dialects, aspect mapping   (today: models.rs)
src/ops.rs            the verbs: take refs in → Job → router → artifacts stored as takes → Vec<Take>
src/local/            raster.rs (crop, resize, pad, rotate, flip, flatten, colour-match) · svg.rs (clean, recolour, sanitise) · probe.rs (ffprobe, video_frame)
src/compose/          ad.rs (AdSpec, Scene, render_ad, render_motion) · lockup.rs · text.rs (measure, wrap, fit) · fonts.rs (fontdb, Fontsource add)
src/sheet.rs          look, contact_sheet
src/live.rs           LiveJob → <studio>/live/*.json|svg (a Progress impl)
src/keys.rs           [env-keys]  KeySpec, resolve, mask, redact (redact is always compiled)
src/preview.rs        [preview]
src/bin/dmm/          main.rs, cli.rs, report.rs, doctor.rs, skill.rs          [cli]
```

Public API, derived one-for-one from the verbs the CLI has today (`still, edit, motion, cutout, upscale, run, vector, vectorize, vector-edit, animate, lockup, ad, sheet, look, import, star, note, ls, show, brand, fonts, models`):

```rust
pub struct Engine { /* router + price cache */ }
impl Engine {
    pub fn new(backends: Vec<Arc<dyn MediaBackend>>, prefs: RouterPrefs) -> Engine;
    pub async fn capabilities(&self) -> Capabilities;                         // union, cached
    pub async fn estimate(&self, studio: &Studio, req: &OpRequest) -> Estimate;
    pub async fn still      (&self, s: &Studio, r: StillReq,      p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn edit       (&self, s: &Studio, r: EditReq,       p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn motion     (&self, s: &Studio, r: MotionReq,     p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn cutout     (&self, s: &Studio, take: &TakeRef, c: &Common, p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn upscale    (&self, s: &Studio, take: &TakeRef, factor: u8, c: &Common, p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn run_raw    (&self, s: &Studio, r: RawReq,        p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn vector     (&self, s: &Studio, r: VectorReq,     p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn vectorize  (&self, s: &Studio, r: VectorizeReq,  p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn vector_edit(&self, s: &Studio, r: VectorEditReq, p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn animate    (&self, s: &Studio, r: AnimateReq,    p: &dyn Progress) -> Result<Vec<Take>>;
    pub async fn model_schema(&self, model: &ModelId) -> Result<ParamSchema>;
}
pub struct Common      { pub params: Map<String, Value>, pub brand: bool, pub cancel: CancelToken, pub tags: TakeTags }
pub struct StillReq    { pub prompt: String, pub models: Vec<ModelId>, pub aspect: Option<Aspect>, pub n: u32, pub refs: Vec<TakeRef>, pub seed: Option<u64>, pub common: Common }
pub struct EditReq     { pub inputs: Vec<TakeRef>, pub instruction: String, pub model: Option<ModelId>, pub aspect: Option<Aspect>, pub n: u32, pub common: Common }
pub struct MotionReq   { pub input: TakeRef, pub prompt: String, pub model: Option<ModelId>, pub seconds: Option<f64>, pub aspect: Option<Aspect>, pub end: Option<TakeRef>, pub common: Common }
pub struct VectorReq   { pub prompt: String, pub model: Option<ModelId>, pub n: u32, pub instructions: Option<String>, pub refs: Vec<TakeRef>, pub effort: Option<Effort>, pub temperature: Option<f64>, pub common: Common }

impl Studio {   // no network
    pub fn open(root: PathBuf) -> Result<Studio>;        pub fn init(root: &Path, name: &str) -> Result<Studio>;
    pub fn find(start: Option<&Path>) -> Result<Studio>; pub fn save_config(&self) -> Result<()>;      // temp file + rename, under the lock
    pub fn takes(&self) -> Result<Vec<Take>>;            pub fn resolve(&self, r: &TakeRef) -> Result<Take>;
    pub fn import(&self, src: ImportSrc, note: &str) -> Result<Take>;
    pub fn star(&self, id: &str, on: bool) -> Result<Take>;  pub fn note(&self, id: &str, text: &str) -> Result<Take>;
    pub fn hide(&self, id: &str) -> Result<Take>;        pub fn lineage(&self, id: &str) -> Result<Lineage>;  // ancestors + descendants
}
pub mod compose { pub fn render_ad(s: &Studio, spec: &AdSpec, stills_only: bool) -> Result<Vec<Rendered>>;
                  pub fn lockups(s: &Studio, icon: &TakeRef, o: &LockupOpts) -> Result<Vec<Take>>;
                  pub fn format_size(name: &str) -> Result<(u32, u32)>;  pub const LAYOUTS: &[&str]; }
pub mod sheet   { pub fn look(s: &Studio, t: &Take) -> Result<PathBuf>;
                  pub fn contact_sheet(s: &Studio, takes: &[Take], o: &SheetOpts) -> Result<PathBuf>; }
pub mod local   { pub fn adjust(s: &Studio, t: &Take, steps: &[Adjust]) -> Result<Take>;
                  pub fn clean_svg(s: &Studio, t: &Take, o: &CleanOpts) -> Result<(Take, CleanReport)>;
                  pub fn export(s: &Studio, t: &Take, preset: &Preset, focus: (f32, f32)) -> Result<PathBuf>; }
```

`TakeRef` keeps today's forms: `t0007`, `7`, `last`, `last-2`; a filesystem path is accepted **only** by `dmm` (the `cli` feature) — in neo a path enters through `media_import`. `SheetOpts { title, numbered: bool, show_model: bool }`: critique sheets are numbered `1…n` with `show_model = false` so Sol scores blind.

## 3. `MediaBackend`, the router, capability-filtered tools

The trait is the one in 08; it lives in the lib so `dmm` and neo share backends.

```rust
#[async_trait] pub trait MediaBackend: Send + Sync {
    fn id(&self) -> &'static str;                                   // "fal" | "quiver" | "starkrouter" | "mock"
    async fn capabilities(&self) -> Result<Capabilities>;
    async fn estimate(&self, job: &Job) -> Option<Usd>;             // None = unknown price
    async fn run(&self, job: Job, progress: &dyn Progress) -> Result<Vec<Artifact>>;
}
pub enum Op { Still, Edit, Motion, Cutout, Upscale, Run, Vector, Vectorize, VectorEdit, Animate }
pub struct Job { pub op: Op, pub model: ModelId, pub prompt: String, pub instructions: Option<String>,
                 pub inputs: Vec<JobInput>,          // { field: Option<String>, role: Source|Reference|EndFrame, bytes, mime }
                 pub aspect: Option<Aspect>, pub n: u32, pub seconds: Option<f64>, pub seed: Option<u64>,
                 pub palette: Vec<String>, pub params: Map<String, Value>, pub cancel: CancelToken }
pub struct Artifact { pub bytes: Vec<u8>, pub ext: String, pub meta: Value,   // seed, request_id, credits, loop_period_ms …
                      pub sent: Value,                                        // request body with inline data replaced by stubs
                      pub cost: Option<Usd> }
pub struct Capabilities { pub ops: BTreeSet<Op>, pub models: Vec<ModelCaps>, pub streaming_drafts: bool,
                          pub arbitrary_endpoints: bool, pub schema_discovery: bool, pub exact_cost: bool }
pub struct ModelCaps { pub id: ModelId, pub alias: Option<String>, pub ops: BTreeSet<Op>, pub max_n: u32, pub max_refs: u32,
                       pub aspects: AspectDialect, pub durations: Vec<f64>, pub audio: bool, pub seed: bool, pub blurb: String }
pub struct Usd { pub amount: f64, pub basis: PriceBasis }           // Exact | LivePrice | PackTable | TokenGuess
pub trait Progress: Send + Sync { fn status(&self, job: &JobId, s: Status);        // Queued{position} | Running{log} | Thinking{text} | Drawing{bytes} | Downloading
                                  fn draft(&self, job: &JobId, index: usize, svg: &str);
                                  fn done(&self, job: &JobId, outcome: &Outcome); }
```

A backend does **network only**: it never touches a studio. `ops` resolves take refs to bytes (SVG rasterised, video → first frame, as `raster_data_uri` does today at 1024–2048 px), builds the `Job`, asks the router, stores each `Artifact` as a take with parents. `Usd` crosses into neo as 08's `Usage { usd: Exact | Estimated }` (`Exact` only when `basis = Exact`).

**Router** — for each job, in this order, stop at the first hit:
1. **Explicit model** in the call. A catalog alias names its backend; a raw id containing `/` is a fal endpoint; `arrow-*` is Quiver. Backend missing → `NotConfigured { needs: "FAL_KEY" }`, never a silent substitute.
2. **User preference**: Settings → Media → per-op ordered model list (`media.prefer.still = ["nano-banana","flux-ultra"]` …) and `media.backend_order`. The first entry whose backend is configured and whose `ModelCaps` fits (op, ref count, aspect, duration) wins.
3. **First capable backend** in registration order (`starkrouter`, `fal`, `quiver`) using that backend's default model for the op.

**Capability-filtered tools.** At task start `neo-media` snapshots `Engine::capabilities()` (cached; refreshed at launch, on key change, every 24 h). A tool is registered only if some configured backend covers its op: no Quiver key → no `media_vector*`/`media_animate`; no fal key → no `media_still/edit/cutout/upscale/motion/run`. Local tools need no backend. The list is then fixed for the task (03 §Context).

## 4. Backends

### `fal` (from `src/fal.rs`, `models.rs`, `ops.rs`)

| | |
|---|---|
| Config | `{ queue_base, api_base, schema_base, credential }` — defaults `https://queue.fal.run`, `https://api.fal.ai`, `https://fal.ai`; no URL constant outside the impl (08) |
| Auth | header **`Authorization`**, scheme `Key`. Sent only to the queue host and the API host |
| Submit | `POST {queue_base}/{endpoint_id}` with the JSON body → `request_id`, `status_url`, `response_url`, `cancel_url` *(cancel_url: verify)* |
| Poll | `GET {status_url}?logs=1` — **always the `status_url` from the submit reply.** Sub-path endpoints (`fal-ai/kling-video/v2.5-turbo/pro/image-to-video`) poll at the app path; the full path answers 405. Delay 700 ms, +300 ms per poll, capped at 4 s. `200` and `202` are both fine. `IN_QUEUE` → `Queued{queue_position}`, `IN_PROGRESS` → `Running{last log line}`, `COMPLETED` → fetch. Network errors while polling are skipped. Hard timeout 25 min |
| Result | `GET {response_url}`; every media URL is found by walking the JSON for objects with a `url` whose `content_type` is `image/ video/ audio/` or whose extension is a media one; `data:` URIs accepted. Kept in `meta`: `seed`, `description`, `prompt`, `has_nsfw_concepts`, `timings` |
| Host pinning | `status_url` / `response_url` / `cancel_url` must be `https` on the queue host, else `OffHost` and the credential is not sent. Result downloads carry **no** auth header, `https` only, 3 tries, 200 MB cap |
| Inputs | inline `data:` URIs in the body (no storage upload). The ledger stores `<N bytes inline>` stubs |
| Catalog | stills `nano-banana` (`fal-ai/nano-banana-pro`), `flux-ultra`, `ideogram`, `recraft`, `seedream`; edit `nano-banana-edit`; motion `kling`, `veo`, `seedance`, `hailuo`; tools `cutout` (`fal-ai/bria/background/remove`), `upscale` (`fal-ai/clarity-upscaler`). Each alias has its own body dialect: `aspect_ratio` enum vs `image_size` enum vs exact pixels (seedream: ~4 MP, multiples of 16), duration as `"5"`/`"10"`, `"4s"`/`"6s"`/`"8s"`, or seconds; nearest aspect chosen in log space; recraft gets the palette as exact `colors`; with references `nano-banana` switches to its edit endpoint; end frame is `tail_image_url` for kling, `end_image_url` otherwise |
| Batching | models that return several images per request get `n` in one call; the rest get `n` parallel calls. Shoot-out = all models in parallel (`JoinSet`); partial failure returns the takes that worked plus `warnings[]`; only all-failed is an error |
| Schema | `GET {schema_base}/api/openapi/queue/openapi.json?endpoint_id=…` (no credential) → the `*Input` schema summarised to `{name,type,required,default,enum,description}` |
| Key check | `GET {api_base}/v1/models/usage?limit=1`: **401 = bad key; 403 = valid key without admin scope = pass**; any other status = pass |
| Price | live per-endpoint price from the fal platform pricing endpoint *(verify in the first week of M6)* → `LivePrice`; else the pack's dated `prices.json` → `PackTable`; else `None` (always confirms, §10). Unit price × images, or × seconds for video |
| Cancel | kill switch / task cancel → `PUT cancel_url`, stop polling, outcome `Cancelled`. A job already `IN_PROGRESS` may still bill; the trace says so |

Errors map to `MediaError`: 401/403 `Auth` · 404 `UnknownEndpoint` · 422 `BadParam { detail }` (the tool result tells Sol to call `media_model_schema`) · 429 `RateLimited` · 5xx `Provider` · `Timeout` · `NoMedia` · `OffHost` · `Cancelled`. Bodies pass through `redact()` and are cut to 1,200 chars. **Retry:** a submit whose outcome is unknown (connection dropped after send) is never retried — that is a double spend; a submit that failed cleanly with 429/5xx is retried once after 2 s; polls and downloads retry freely.

### `quiver` (from `src/quiver.rs`, `ops.rs`)

| | |
|---|---|
| Config | `{ api_base = https://api.quiver.ai/v1, credential }` |
| Auth | header **`Authorization`**, scheme `Bearer`; request header `Accept: text/event-stream`; the response header `x-request-id` is recorded |
| Endpoints | `POST /svgs/generations` (`model, prompt, n ≤ 16, instructions?, references[{base64}]`) · `/svgs/vectorizations` (`image{base64}, auto_crop`) · `/svgs/edits` (`svg, prompt, reference_images[≤4], max_review_steps: 1`) · `/svgs/animations` (`svg_source{base64}, prompt?`) · `GET /models` (key check **and** capabilities: `supported_operations` per model). Optional on all: `reasoning_effort` (`low…xhigh`), `temperature` |
| SSE | `stream: true` is forced. Events are `data:` lines joined until a blank line; `[DONE]` ends. `reasoning`/`generating` → `Thinking`; `draft` (`update_type: delta` appends, otherwise replaces, per `index`) → `Progress::draft` with the partial SVG closed at its last complete tag; `content` = one finished SVG (+ `credits`, `loop_period_ms`, `opening_animation_ms`, `id`); `usage` kept from whichever event carries it; an `error` event fails the job. A stream that only sent drafts yields the last draft containing `</svg>`; no SVG at all → `Stream` error carrying the request id. A non-stream JSON reply (`data[].svg`) is accepted |
| Models | `arrow-2` (generate, vectorize, edit, animate; token-priced) · `arrow-2-telos` (higher fidelity, 1.5× the rate) · `arrow-1.1` (flat price per SVG; **generate + vectorize only**). Router rule: `VectorEdit`/`Animate` never go to a model whose live `supported_operations` lacks them |
| Errors | 401/403 `Auth` ("rejected, or the key lacks this model/operation") · 402 `Balance` (with the billing URL) · 429 · 5xx. A dropped stream is **not** retried (it may have billed); the tool result says "unknown outcome" |
| Price | pre-flight `TokenGuess` from a rolling median of this machine's own `usage` per (model, op), seeded from the pack table; after the call, tokens × the model's rate ÷ `n` is written to each take (`est_usd`) — or `credits` when the API reports them |
| Quirk | **arrow-2 bakes a full-canvas background `<rect>` into icons** → §8 step F3 |

### `starkrouter` (later, K2)
One backend whose `capabilities()` is the union of the other two; `{ base_url, credential }` from the `starkrouter` Keychain account; fal queue passthrough and Quiver SSE passthrough preserved (08); `exact_cost = true`, so `Usd.basis = Exact` and the spend meter stops estimating. Registered first; direct keys remain an advanced option. No tool, ledger or pipeline change — that is the test of this seam.

## 5. Studio, takes ledger, lineage (A16)

A **studio** is a plain folder under `~/Documents/starkbot-neo/studios/<name>/`; SQLite stores only the studio path and take ids.

```
studio.json    name + brand kit (§9)               takes/    t0001.png  t0002.svg  t0003.mp4 …   (never rewritten)
takes.jsonl    append-only ledger                  ids/      one empty file per reserved id
takes.lock     advisory lock file                  canvas/   hypercanvas document (11; neo only)
exports/       deliverables, by date/preset        ads/<name>/  spec.json + <format>.{svg,png,html,mp4}
sheets/        contact sheets, looks, frames/      live/     in-flight jobs      fonts/  studio fonts      .gitignore
```

**Take** (one ledger line): `id, kind (image|svg|video|audio), file, op, model, prompt, parents[], width, height, seconds, took_s, created, params, meta, note, starred` — exactly today's struct — plus neo's additions, all optional: `backend, seed, cost {usd, basis}, task, brief, round, score {…}, hidden, source_name`. `op` ∈ `still · edit · motion · cutout · upscale · run · vector · vectorize · vector-edit · animate · lockup · import · adjust · clean`. **Lineage** is the DAG over `parents[]`; "branch from here" is just a new take naming that parent.

**Sharing one studio between neo and `dmm`:**
1. **Append-only.** A take file is written once. Star, note, score, hide = a new full line for the same id; readers fold, last line wins. Nothing is deleted: `hidden: true` is the tombstone; purging is a user action in Finder.
2. **Ids** are reserved by `create_new` on `ids/tNNNN` (atomic across processes) — already how `dmm` works.
3. **Appends**: take an exclusive `flock` on `takes.lock`, write the whole line with a single `write_all` on an `O_APPEND` handle, `sync_data`, release. `studio.json` is written temp-file + `rename` under the same lock. Locks are held for microseconds, never across a network call.
4. **Unknown fields survive.** `Take`, `Brand` and `StudioConfig` carry `#[serde(flatten)] extra: Map`, so an older `dmm` re-writing a line for a star keeps neo's fields. *Required upstream change* — today's struct would drop them.
5. **Order**: write `takes/<id>.<ext>` → append the line. Readers ignore a file without a line and skip a line that does not parse (torn tail after a crash).
6. neo watches `takes.jsonl` (`notify`, 200 ms debounce) and emits `MediaTakeCreated` for lines it did not write; both programs write `live/`, so either board shows the other's jobs.
7. `dmm` never reads `canvas/`; neo never needs `dmm`'s `.env`.

## 6. `neo-media`: the adapter and Sol's tools

`crates/neo-media`: `engine` (builds `Engine` from `neo-keys` credentials + settings), `tools/*` (metalcraft `Tool`s wrapped in `Gated<T>`), `spend`, `events` (`Progress` → `AppEvent::MediaJobStarted | MediaJobProgress | MediaDraft | MediaTakeCreated | MediaJobFailed | MediaSpend`, teed into `LiveJob`), `pipeline` (brief, rubric, presets — data loaded from the pack), `enable`, `doctor`, `handoff` (pasteboard, upload staging). Depends on `neo-core`, `neo-keys`, `neo-store`, `degen-media-maker`. It does **not** depend on `neo-canvas`: the canvas subscribes to events.

Every tool takes `why` and an optional `studio` (default: the active one). Tools pass and return **take ids, never bytes**. Standard return: `{ takes:[{id,kind,w,h,seconds?,model,cost_usd}], sheet?, spent_usd, task_spent_usd, warnings[] }`.

| Tool | Args | Does | Gate |
|---|---|---|---|
| `media_still` | `prompt, models?[], n?=1, aspect?, refs?[], seed?, brand?=true, params?` | text → image; several models = a shoot-out; returns a numbered sheet when > 1 take | spend |
| `media_edit` | `takes[], instruction, model?, aspect?, n?` | relight / restyle / composite / fix | spend |
| `media_cutout` | `take` | background removal → transparent PNG | spend |
| `media_upscale` | `take, factor?=2 (2\|4)` | detail upscale | spend |
| `media_motion` | `take, prompt, model?, seconds?, aspect?, end_take?` | image → video (SVG rasterised first) | spend, **always confirms** |
| `media_run` | `endpoint, prompt?, inputs{field: take}, params?` | any fal endpoint | spend; unknown price → confirm |
| `media_vector` | `prompt, model?, n?=4, instructions?, refs?[], effort?` | text → SVG | spend |
| `media_vectorize` | `take, model?, auto_crop?` | raster → SVG | spend |
| `media_vector_edit` | `take(svg), instruction, refs?[≤4]` | SVG + instruction → SVG | spend |
| `media_animate` | `take(svg), prompt?` | SVG → animated SVG | spend |
| `media_adjust` | `take, steps[]` of `crop · resize · pad · rotate · flip · flatten(bg) · recolor(svg) · color_match(palette) · clean_svg` | local, instant, each call a new take | none |
| `media_lockup` | `icon_take, name, tagline?, font?, weight?, color?, tracking?, uppercase?` | horizontal / stacked / badge lockups, **text outlined**, 3 SVG takes | none |
| `media_ad` | `name, layout (hero\|center\|split\|poster), bg_take, logo_take?, copy{eyebrow,headline,sub,cta}, formats[], focus?, scrim?, motion?` | **Set hand-off**: validates assets and copy, writes `ads/<name>/spec.json`, emits `MediaSetRequested` — the canvas instantiates a **Set** frame (master + placements) and owns its rendering and export (11). Headless (`neo media ad`, `dmm ad`) and before M7 the dmm compositor renders the formats instead | none |
| `media_sheet` | `takes[], title?, numbered?=true, show_model?=false` | one comparison PNG | none |
| `media_look` | `take \| sheet \| takes[≤12]` | **returns the image to Sol's vision** (§7) | none |
| `media_import` | `source: path \| url \| pasteboard \| selection, note?` | bring a file in as a take. `path` only if it came from an OS open panel, a drop or a selection; `url` = http(s) GET, image/video types, 50 MB cap | pre-gate for `url` |
| `media_export` | `takes[], preset \| {w,h,format,quality}, dest: exports \| folder(path) \| clipboard \| upload_staging, focus?` | produce deliverables (§8 F7) | none inside the studio; **pre-gate** outside it |
| `media_ls` · `media_show` · `media_lineage` | `filter?{kind,op,starred,since}, limit?` · `take` · `take` | ledger reads | none |
| `media_star` · `media_note` · `media_hide` | `take, on? \| text` | ledger appends | none |
| `media_models` | `query?, op?` | catalog + live capabilities + price where known | none |
| `media_model_schema` | `model` | live parameter schema (after a `BadParam`) | none |
| `media_brand` | `get \| set{field: value}` | read or edit the brand kit | none |

**Gate policy.** Paid tools use `GatePolicy::Spend`: deterministic rules (§10) replace Jev's `spends` head because the cost is computed, not guessed; Jev's `on_task` head still runs, so text inside an imported image or page cannot induce a spend. `media_export` outside the studio and `media_import(url)` take the normal pre-gate. Anything that publishes is the navigator's `outward` gate, never this crate's.

**Concurrency (A10).** Media work is canvas-class: it runs concurrently with other work and never holds the desktop worker. A hand-off step (§13) is queued to the desktop worker like any other desktop action. At most 6 backend jobs in flight per studio.

## 7. `media_look` and image parts in tool results

Sol judges with its own vision: *generate → look at one sheet → score → iterate*. Requires **metalcraft 0.12: image parts in `ToolResult`**. `media_look` returns `[text, image]`: the text part lists what is shown (`1 = t0041 1080×1350`, …); the image part is a PNG/JPEG, long edge ≤ 1568 px, ≤ 1.5 MB (re-encoded to JPEG q85 if larger).

- One take → the take itself; SVG on a checker at 1200 px; video → a 6-frame filmstrip (3 × 2, timestamps) — all existing `sheet::look` behaviour.
- Several takes → a contact sheet: 520 px cells, 1–4 columns by count, cell shape from the average aspect, numbered, model names hidden.
- Small-size check: `media_look(take, sizes:[16,32,64])` renders an icon at real favicon sizes on light and dark.
- Trace: looks are stored once as files under `sheets/`; the trace references them, and older look images are replaced by `[look elided]` at batch eviction (03).
- If 0.12 slips: inject the sheet as an `input_image` user message straight after the tool result. Same tool contract.

Jev judges text, not pixels; it never scores media. Jev's part here is intent/route at intake, `on_task`, and the export pre-gate.

## 8. The quality pipeline (precise procedure)

Run by Sol from the pack skill `media-pipeline`; enforced where marked **[code]**. "Make me a quick picture of X" takes the short path (B → one model × 2 → look → deliver); anything called a logo, ad, campaign, hero, thumbnail or "for the site" takes all of it.

**A. Brief** — a JSON object written first, posted to the Conversation, stored at `canvas/briefs/<id>.json`, amendable by voice ("warmer, less corporate" rewrites fields, not the whole brief):
```json
{ "id":"b0007", "deliverable":"logo|icon_set|hero|product_shot|social_card|ad_set|thumbnail|illustration|animated_mark|motion",
  "subject":"", "audience":"", "message":"", "mood":[""], "palette":["#0B0B10"], "composition":"", "style_refs":["t0012"],
  "must_have":[""], "must_not":[""], "copy":{"eyebrow":"","headline":"","sub":"","cta":""},
  "targets":["ig_feed_portrait","og"], "master":{"aspect":"4:5","min_px":2160}, "budget_usd":0.60, "quality":"quick|standard|best" }
```
Defaults come from the brand kit and `soul.md`; a missing subject or target is one `ask_user`, never a guess.

**B. Art direction per model** — Sol loads `prompt-<family>` skills before writing prompts and writes **one prompt per model**, never a shared one. Skills shipped in the pack: `prompt-nano-banana` (long natural-language direction, reference composition), `prompt-flux` (photographic vocabulary, lens and light, `raw`), `prompt-ideogram` (graphic/poster styles, presets), `prompt-recraft` (style ids; palette goes as exact colours, not words), `prompt-seedream` (exact pixels, product shots), `prompt-motion` (kling camera moves 5/10 s · veo audio 4/6/8 s 16:9|9:16 · seedance 2–12 s any aspect, `camera_fixed` · hailuo physical motion 6/10 s), `prompt-arrow` (subject in `prompt`, style in `instructions`, flat shapes, colour count, "no text"; effort levels; telos for complex illustration; 1.1 cannot edit or animate). Each skill has: what the model is good at, prompt skeleton, three worked examples, failure patterns, parameters worth setting. Every generation prompt ends with the pack's no-text clause unless the brief says text is *in the scene*.

**C. Shoot-out policy**

| Deliverable | `standard` | `best` |
|---|---|---|
| logo / icon / mark | `arrow-2` × 4 | + `arrow-2-telos` × 2, + `recraft` × 2 → vectorize the winner |
| hero / photo / product | `nano-banana`, `flux-ultra`, `seedream` × 1 | × 2 each |
| illustration / social graphic | `recraft`, `nano-banana`, `ideogram` × 1 | × 2 each |
| product with references | `nano-banana` (refs → edit endpoint) × 2, `seedream` × 1 | × 2 each |
| motion | **one** model chosen by need (audio → veo · 1:1 or 3:4 → seedance · camera move → kling · physical → hailuo) | two models only if the user asks |

`quick` = the first model in the row × 2. The estimate for the whole shoot-out is shown before it starts (§10). At most **2 shoot-outs per brief** without asking the user. **[code]**

**D. Critique** — `media_look` on a numbered, model-blind sheet. Sol scores each take 0–5 per criterion and posts a one-line reason per take:

| Criterion | Weight | 5 looks like |
|---|---|---|
| Brief fit (subject, message, must-haves) | 30% | everything asked for, nothing forbidden |
| Composition (focus, hierarchy, room for copy) | 20% | clear focal point; clean area where the layout needs text |
| Brand fit (palette, style, mood) | 15% | reads as the same brand as the style refs |
| Technical (anatomy, edges, artefacts, noise) | 15% | survives 100% zoom |
| Small-size legibility | 10% | reads at 64 px (icons: 16 px) |
| Originality / appeal | 10% | not the stock answer |

**Hard fails (score 0, never delivered):** any model-rendered text or pseudo-text the brief did not ask for; watermark or signature; malformed hands/faces/products; a `must_not` present; for SVG: raster `<image>` inside, > 12 distinct fills on a logo, or a background rect still present. **Pass** = weighted ≥ 4.0 **and** no criterion < 3. Passing takes are starred and scores written to the ledger. **[code: `media_star`/score append]**

**E. Iterate** — edit the winner, do not re-roll: `media_edit` / `media_vector_edit` with one specific instruction taken from the lowest-scoring criterion; look again. **Max 3 edit rounds per winner [code]**; still failing → post the best two with their scores and ask. A budget hit also asks (§10).

**F. Finish**, in order, only the steps that apply:
1. `media_cutout` if the layout needs a subject without its background.
2. `media_upscale` if the master's short side < `master.min_px` (2× first; 4× only when still short).
3. **SVG cleanup** (`clean_svg`, local and free) **[code]**: sanitise (remove `<script>`, `on*` attributes, `<foreignObject>`, external `href`s — provider SVG is untrusted input to a webview) → remove a first-child full-viewBox background `<rect>` when its removal leaves the artwork intact → normalise to `viewBox="0 0 W H"` with no fixed px size; icons padded to a square → strip metadata and editor namespaces, collapse empty groups, round coordinates to 2 dp → snap fills within ΔE 6 of a brand colour to that colour → report distinct fills and path count. When the baked rect is interleaved with the art and cannot be removed locally, follow with `media_vector_edit "remove the background rectangle; keep everything else identical"`.
4. Colour-match rasters toward the palette only when brand fit scored < 4 (`color_match`, strength ≤ 0.35).
5. **Text.** Wordmarks → `media_lockup` (compositor, outlined paths, no font dependency). Headlines, CTAs and all copy → a Graphic/Set frame, set in HTML/CSS with brand fonts by the canvas renderer (A13); headless → the dmm compositor (`text::fit` with real font metrics). An image model never sets deliverable text. Text *inside the scene* (a shop sign) only on explicit request, and Sol must read it back exactly in the look or it is a hard fail.
6. Final look of the finished piece at delivery size **and** at thumbnail size.
7. **Deliver** with `media_export` presets. **[code]** Rasters: cover-crop around the `focus` point, Lanczos3, sRGB, EXIF stripped, PNG when alpha else JPEG q90 (WebP on request). A preset with a byte budget steps quality down to fit. Pieces with copy are exported from their Set frame by the canvas, using the same table. Output: `exports/<date>/<brief-or-name>/<preset>.<ext>`.

| Preset | Pixels | Preset | Pixels |
|---|---|---|---|
| `x_post` · `x_square` · `x_header` | 1600×900 · 1080×1080 · 1500×500 | `yt_thumb` (≤ 2 MB) · `yt_banner` | 1280×720 · 2560×1440 |
| `li_post` · `li_square` · `li_cover` | 1200×627 · 1200×1200 · 1128×191 | `og` · `link_banner` | 1200×630 · 1200×628 |
| `ig_square` · `ig_feed_portrait` · `ig_story` | 1080×1080 · 1080×1350 · 1080×1920 | `tiktok` · `pinterest` | 1080×1920 · 1000×1500 |
| `meta_feed` · `meta_feed_45` · `meta_story` · `meta_link` | 1080×1080 · 1080×1350 · 1080×1920 · 1200×628 | `ph_gallery` · `ph_thumb` | 1270×760 · 240×240 |
| `gads_landscape` · `gads_square` · `gads_portrait` · `gads_logo` · `gads_logo_wide` | 1200×628 · 1200×1200 · 960×1200 · 1200×1200 · 1200×300 | `app_icon` | 1024×1024 master + macOS iconset 16/32/128/256/512 @1×/@2× |
| `favicon_set` | `favicon.svg`, `favicon.ico` (16/32/48), `apple-touch-icon` 180, 192, 512 | `print_a4` | 2480×3508 |

The table is data (`presets.json` in the pack), platform limits change; presets `square · feed · story · landscape · banner` stay as aliases for `dmm` compatibility.

## 9. Consistency tools

**Brand kit** — `studio.json › brand`; the first nine fields are today's `dmm` schema, the rest are additive:
```json
{ "name":"", "tagline":"", "palette":["#ink","#accent","#paper"], "display_font":"", "body_font":"", "style":"", "voice":"", "logo":"t0008", "website":"",
  "palette_roles":{"ink":"#0B0B10","accent":"#FF4D2E","paper":"#F6F2EA"}, "negative":"no gradients, no stock-photo smiles",
  "style_refs":["t0012","t0019"], "subject_refs":{"product":["t0031","t0032"],"mascot":["t0040"]},
  "logo_variants":{"horizontal":"t0050","stacked":"t0051","badge":"t0052"} }
```
- **Auto-applied** unless `brand:false`: `style` + palette appended to raster prompts ("Art direction: …"); Quiver gets "Use only these brand colours: …" in `instructions`; recraft gets exact `colors`. The canvas derives its tokens from the same kit (11).
- **Style refs**: up to 4 pinned takes passed as references wherever `ModelCaps.max_refs > 0`. **Subject refs**: named sets for a product or character; Sol passes the set whenever the brief names it.
- **Seeds**: recorded in the ledger whenever the backend returns one; `media_still(seed)` reproduces; a "same look" follow-up = same model + seed + style refs with only the subject clause changed. Quiver has no seed — consistency there comes from references, `instructions`, and low `temperature`.
- Fonts: `fonts/` in the studio, then `~/Library/Application Support/com.starkbot.neo/fonts`, then system; `neo media fonts add "Family"` fetches static TTFs from Fontsource. A missing brand font is a warning in the tool result, never a silent fallback in a deliverable.

## 10. Spend control (K5)

`SpendGuard::check(estimate)` runs before every paid call; the ledger records the actual afterwards; `spend.kind` gains `fal | quiver | starkrouter`.

| Rule | Default | Effect |
|---|---|---|
| per media call | **$0.25** | estimate ≤ limit → runs, the amount is in the action sentence (*"Generate 3 images with flux-ultra (~$0.18)"*); above → confirm card |
| unknown price | — | always a confirm card ("price unknown") |
| `media_motion` | — | always a confirm card the first time in a task, whatever the price |
| per task | **$1.00**, all spend kinds together | an estimate that would cross it → confirm card offering a one-time raise **for this task only**; denied → Sol delivers the best it has |
| per day | cap from Settings → Safety | crossing → paid media tools return `denied: daily cap` until the user raises it; the queue keeps running for free work |

A shoot-out is estimated and confirmed **as one unit**. Estimates are never split to get under a limit **[code: one estimate per tool call, and a tool call cannot be repeated within 10 s with the same args to dodge it]**.

**Confirm card content:** action sentence · models × count · estimate and its basis ("live price" / "price table dated …" / "guess from your past usage") · task spend so far / task cap · today / daily cap · thumbnails of the takes that will be uploaded and the host they go to · buttons **Approve** · **Cheaper** (the engine's alternative: fewer variants or the cheapest capable model, with its price) · **Deny**. 2-minute timeout = deny. Voice "yes/no" resolves it like any confirm card.

## 11. The embedded `neo-media` pack

Ships **in every build, disabled** (`include_dir!`). Until enabled, none of its tools are registered, its skills are not in the index, and the canvas shows an "Enable media" card wherever generation would be offered; local, free features (import, adjust, lockup, export) are also off, so the pack is one switch. Contents: `agent_pack.json` (`native_tools: ["media_*"]`), `integrations/fal` and `integrations/quiver` (`requires_env: FAL_KEY` / `QUIVERAI_API_KEY`, `allowed_hosts`, `key_help` with the get-a-key URL and validation rule), skills (`media-pipeline`, `prompt-*`, `media-quirks`, `svg-cleanup`, `export-presets`), data (`prices.json`, `presets.json`, `rubric.json`), persona `art-director`.

**Enablement flow** (06's generic flow; this is the worked example). Entry points: Settings → Packs → `neo-media` → Enable · the canvas card · Sol's `ask_user` offer when a task needs it ("Media tools are off. Want to set them up? You'll need a fal.ai key and a QuiverAI key." — a yes **opens** the flow; the task waits in `waiting_user`).

1. **What it does** — capabilities; that calls cost money on the user's own fal and Quiver accounts; consent summary: hosts (`queue.fal.run`, `api.fal.ai`, `fal.ai`, fal's media CDN, `api.quiver.ai`, `api.fontsource.org`), paid tools, writes to the studios folder, and that looks are sent to the inference provider for critique.
2. **fal.ai key** — `KeyField` + "Get a key" → live validation → Keychain account `FAL_KEY` · **Skip** allowed.
3. **QuiverAI key** — same, validated by `GET /models` → `QUIVERAI_API_KEY` · **Skip** allowed (at least one of the two is required).
4. **Spend limits** — per-call, per-task, per-day, pre-filled.
5. **Doctor** — ffmpeg, fonts, studios folder writable, reachability → creates the `scratch` studio → enabled.

**Key validation rules.** Trim; accept `dmm`'s aliases on paste (`FAL_API_KEY`, `QUIVER_API_KEY`, `QUIVER_AI_KEY`) and a pasted `NAME=value` line. fal: **401 → invalid; 403 → valid**; 200 → valid; network error → "could not check", saving allowed with a warning badge. Quiver: 200 → valid, and the model/operation list is shown; 401/403 → invalid; 402 → valid key, empty balance (warned). A key is never echoed back, logged, or sent to the webview after entry (`neo-keys` holds the only copy).

**Never by voice.** Keys are entered only in `KeyField`; the final **Enable** click is a pointer/keyboard action, not a voice confirm; Sol and the navigator never type, read or paste a key. **Partial enablement**: fal only → raster/video tools; Quiver only → vector tools; the pack detail shows which half is live and offers "Add the other key". A waiting task is **re-queued at the front and restarted**, because the tool list is fixed per task. Disable keeps keys unless "also remove keys" is ticked; studios are never deleted.

## 12. Selection is context ("that one")

Every intake and every media/design task carries these facts: `studio`, `selected_takes` (takes behind the canvas selection, in selection order), `last_created_takes`, `last_sheet` (number → take id), `focused_frame`. Resolution order for a reference **[code, before Sol is asked]**: explicit id ("t forty-two") → ordinal against `last_sheet` ("the second one") → `selected_takes` → `last_created_takes` when it has exactly one → otherwise `ask_user` showing the numbered sheet. Sheets are numbered precisely so that spoken ordinals are unambiguous.

## 13. Media meets the browser and the desktop

| Hand-off | How |
|---|---|
| **Clipboard** | `media_export(dest: clipboard)` writes `NSPasteboard`: `public.png` + file URL (raster), `public.svg-image` + UTF-8 source (SVG), file URL (video). Paste is a desktop step |
| **Web upload** | `media_export(dest: upload_staging)` writes to `exports/` and adds the path to the task's **upload allow-list**; the navigator's upload operation calls CDP **`DOM.setFileInputFiles`** with that path. The navigator may set **only** allow-listed paths — this is the whole of its file access (P3). Posting is the navigator's `outward` confirm |
| **OS open panel** (native apps, M10) | routine in `neo-apple-apps`: `cmd+shift+g` → type the staged path → Return |
| **"Use this"** | browser: the navigator's snapshot gives the image URL → `media_import(url)`. Desktop: drop onto the canvas, paste, or the app's own `NSOpenPanel`; from M10, Finder selection via `AXURL` |
| **"Put it on my desktop"** | `media_export(dest: folder("~/Desktop"))` — pre-gate, then a plain file write by `neo-media` |

## 14. What the canvas must provide (parity checklist for 11)

Takes as nodes on a Board, newest first · in-flight jobs with queue position and **streaming SVG drafts** · star / note / hide · lineage strip with "branch from here" · viewer: zoom, checkerboard for alpha, video scrub, animated SVG playback, A/B wipe · instruction bar against the selection → `media_edit` / `media_vector_edit` · local edits as new takes (`media_adjust`) · Set frames from `media_ad` specs, re-running a name updates it · brand kit panel bound to `studio.json` · studio switcher · drag a take out to Finder or any app · the "Enable media" card · spend meter and confirm cards.

## 15. ffmpeg, Doctor, build notes

- **ffmpeg / ffprobe** are found on `PATH` plus `/opt/homebrew/bin` and `/usr/local/bin` (a GUI app does not inherit the shell's `PATH`); never bundled in v1. Without them: motion renders, filmstrips, video dimensions and video import posters are off; `media_motion` still works and its look is a plain notice. **Homebrew ffmpeg has no libass/drawtext** — no `subtitles`, `ass` or `drawtext` filters are ever used; text and captions are drawn by our renderer as PNG layers and composited with `overlay` (+ `enable=between(t,a,b)`). Encode: `libx264 -crf 17 -pix_fmt yuv420p -movflags +faststart`, audio `aac` when the clip has it.
- **Doctor checks** (`neo doctor`, the flow's last step, Settings → Packs): pack state · each key: present / validated / last error · Quiver models + operations · fal price source (live or table date) · ffmpeg + ffprobe versions, `libx264` and `overlay` present · brand fonts resolve · studios folder writable + free disk ≥ 2 GB · a 256 px resvg render < 50 ms (catches an unoptimised build).
- **Build**: pixel work is unusably slow unoptimised. Workspace `Cargo.toml`: `[profile.dev.package."*"] opt-level = 3`, and explicitly `opt-level = 3` for `degen-media-maker` and `neo-media` (workspace members are not covered by `"*"`). Tests touching pixels run under the same profile. New lib deps: `fs4` (flock), `notify`, an XML reader/writer for `clean_svg`, `image` with `ico`; `webp` crate for lossy WebP (`image` is lossless-only).
- Large files stay on disk; the webview loads them through Tauri's asset protocol scoped to the studios folder; the DB and the trace hold take ids.

## 16. Video scope

In this engine: image → video takes (fal), animated SVG (Quiver), motion renders of an `AdSpec` over a still or a clip (keeps the clip's audio), filmstrips, first/mid-frame extraction. **Not** here: timelines, multi-clip edits, captions tracks, audio mixing — those are Video frames in the canvas (M12), which call this engine for clips and frames. Beyond motion-graphics scale (trim, reframe, colour, long-form) → **starflux** (`~/ai/starflux`) as the later native-tool pack **`neo-video`**, exchanging takes through the same studio folder.

## 17. Privacy

Prompts and input images go to fal / Quiver under the user's own keys; inputs travel inline in the request, not to third-party storage. Looks and sheets go to the inference provider so Sol can critique — a studio marked **private** disables `media_look` on imported takes and Sol asks the user to pick instead. Imports record `source_name` (basename), not the full path. Exports strip EXIF/XMP. Provider error bodies are redacted against every known secret before they reach a log, a trace or Sol. No telemetry. `takes.jsonl` contains prompts in clear text — it is the user's folder, stated in the flow.

## 18. Test plan (Rust only)

- **Mock backend** (`backend::mock`, scripted: fixture artifacts, progress, drafts, prices, failures) drives every tool, the router (explicit / preference / first-capable / not-configured), capability filtering, partial shoot-out failure, cancel, and `SpendGuard` (thresholds, unknown price, task-cap raise, daily cap, anti-splitting).
- **Wire tests** against local `axum` stubs: fal submit → sub-path `status_url` → 202 → `COMPLETED` → result; off-host `status_url` refused with no credential sent; 401/403/404/422 mapping; dropped submit is not retried. Quiver SSE: delta drafts, replace drafts, drafts-only stream, non-stream reply, mid-stream `error`, 402. Key validation table (401 / 403 / 200 / offline).
- **Ledger**: 8 threads + 2 processes append 500 takes → unique ids, every line parses, fold is correct; unknown fields survive a star written by a struct without them; torn last line is skipped; `dmm` fixtures from real studios load unchanged.
- **Golden composites**: lockups, the 4 ad layouts × 5 formats, contact sheets, SVG looks — rendered with a bundled test font (never system fonts); SVG output compared as text, PNG with tolerance (channel delta ≤ 2, ≤ 0.1% of pixels). `clean_svg` goldens include real arrow-2 icons with the baked rect, and a hostile SVG (script, `onload`, external href).
- **Export**: every preset yields exact pixel size, sRGB, no EXIF, under its byte budget; focus-point crops checked on a marked test image.
- **Release review set** (manual, paid, before each release): `neo media review run` executes ~20 fixed briefs — 4 logos/marks, 2 icon sets, 3 heroes, 3 product shots (with references), 3 social cards, 2 ad sets, 1 YouTube thumbnail, 1 illustration, 1 animated mark — at `standard`, writes sheets, scores, cost and time; `neo media review compare <prev>` builds side-by-side sheets against the previous release. Ship gate: no brief regresses by > 0.5 weighted points, zero hard fails delivered, median cost per brief not up > 20%.

## 19. CLI (`neo media …`, headless, same crates)

`neo media enable | doctor | studios | init <name> | use <name> | brand [k=v…] | fonts add <family> | models [query] | schema <model> | estimate <verb> … | spend [--today]`
`neo media still | edit | cutout | upscale | motion | run | vector | vectorize | vector-edit | animate` (flags as `dmm`: `-m a,b,c  -a 4:5  -n 2  --ref t0003  -p key=value  --no-brand  --yes`)
`neo media adjust | clean | lockup | ad | sheet | look | import | export --preset ig_story,og | ls | show | lineage | star | note | hide`
`neo media pipeline "three logo ideas for Degen Radio" [--quality best] [--budget 1.50]` · `neo media review run | compare <dir>`
Without `--yes` a call over a limit prompts on the TTY; non-interactive without `--yes` → exits `needs_confirm`. `dmm` stays the key-in-`.env` tool for other agents; both open the same studios.

## 20. Milestones

**M6 — Media engine** (needs M4 gates + M5 Sol; metalcraft 0.12 image parts)
1. Upstream: lib + bin split, async, `MediaBackend` + router, `extra`-preserving ledger + lock, `local/`, `SheetOpts`; `dmm` output unchanged on a recorded session.
2. `neo-media`: engine from Keychain, all tools, `SpendGuard`, events, `neo media …`.
3. Pack + enablement flow + Doctor checks.
4. `media_look` with image parts; pipeline skills, rubric, presets, `clean_svg`; review set v1.

*Accepted when:* (a) fresh install: media tools absent; the flow enables with only a fal key and vector tools stay hidden; adding the Quiver key lights them up on the next task; (b) by voice or chat only: "make three logo ideas for Degen Radio" → numbered sheet, blind scores with reasons, winner cleaned (no background rect, passes the 16 px look) → "animate the second one" → "put it on my desktop" → the file is there; spend shown and under the cap; (c) a shoot-out estimated over $0.25 shows the confirm card with **Cheaper**; deny leaves zero provider calls; (d) `dmm ls` in the same studio lists neo's takes, and a `dmm star` does not lose neo's fields; (e) kill switch during a fal job cancels it and the trace says whether it may have billed; (f) no OpenAI image endpoint is referenced anywhere in the workspace (CI grep).

**M7 touchpoints**: canvas subscribes to `Media*` events (takes → Board nodes, streaming drafts); `MediaSetRequested` → Set frame; §14 parity; exports of pieces with copy move to the canvas renderer; `selected_takes` comes from canvas selection. **M9**: GTM workflows call `media_export(upload_staging)` + the navigator upload. **M12**: Video frames pull clips via `media_motion`, frames via `local::probe`; caption layers as PNG overlays; `neo-video` / starflux bridge.

## 21. Risks

| Risk | Answer |
|---|---|
| Async rewrite of a working blocking crate regresses `dmm` | recorded-session parity test; split lands in two PRs (move → async) |
| Two ad renderers (dmm compositor headless, canvas HTML in-app) drift | the canvas is authoritative in-app; compositor output is labelled "headless render"; shared `AdSpec` + shared preset table; goldens on both |
| Price estimates wrong or the fal pricing endpoint unavailable to normal keys | basis shown on every card; unknown → confirm; actuals reconcile the day total; StarkRouter makes it exact |
| Unknown-outcome calls bill without a take | never auto-retried; trace + spend meter mark "possibly billed"; request id kept for support |
| Model catalog churn (endpoint ids move) | catalog + prices + prompt skills are pack data, updatable without an app release; 404 → `media_models` hint |
| Provider SVG as an injection vector in the webview | `clean_svg` sanitise is mandatory before a take is inlined; canvas frames are sandboxed (11) |
| Sol's self-critique is lenient | blind numbered sheets, hard-fail list, fixed weights, release review compared by a human |
| Model text sneaks into deliverables | no-text clause + hard fail + all copy set by our renderer |
| Large inline data URIs rejected by an endpoint | inputs capped at 2048 px long edge; on 413 retry once at 1536 px |
| Studio folder in iCloud-synced `~/Documents` breaks `flock` / causes conflicts | Doctor warns; setting to move the studios root |

## 22. Open questions

1. Daily cap default amount (K5 names the cap, not the number) — proposed $10.
2. Is $1.00/task meant to include media? Proposed above: yes, with a one-time per-task raise on a confirm card.
3. fal live pricing and `cancel_url` availability for non-admin keys — verify in M6 week 1.
4. Bundle a static ffmpeg later (licence + notarization) or stay PATH-only.
5. Should `dmm` also gain an optional Keychain key source so one machine has one copy of each key?
