# Research archive — OpenRouter (2026-09-18)

> **Not part of the plan.** OpenRouter was dropped on 2026-09-18: it cannot carry fal or Quiver. Kept as reference for designing **StarkRouter** (plans/08-providers.md) — its API shapes (PKCE sign-in, `~…-latest` aliases, exact `usage.cost`, provider pinning, `session_id` sticky caching, unified image/video endpoints) are the ones worth copying. The "Design", "Spikes" and "Knock-on edits" sections below are void.

Researched 2026-09-18 against openrouter.ai/docs and the **public live JSON**
(`/api/v1/models`, `/images/models`, `/videos/models`, `/providers`). No
authenticated call was made — behaviour is as documented. *(verify)* = check in
the spike phase.

## 1. Verdict

- **fal and Quiver cannot run through an OpenRouter key.** fal is not an OpenRouter provider — the partnership is the reverse (fal resells OpenRouter LLMs under a *fal* key). `quiver` exists as a **provider slug with zero models** (`…/images/models/quiver/arrow-2/endpoints` → 404): probably pre-launch. Worth re-checking at build time; if Arrow lands there, the Quiver key becomes optional too.
- **But one OpenRouter key covers almost everything else**: Sol inference (with an upstream-maintained `latest` alias), speech-to-text (incl. `gpt-transcribe`), text-to-speech (not OpenAI's voices), 52 image models incl. **true-SVG Recraft vector**, and ~20 video models. Only **Jev** (TypeSafe) can never go through it.
- And it enables something a raw OpenAI key cannot: **"Sign in with OpenRouter"** — OAuth PKCE with a localhost callback, no key pasting.

## 2. Coverage map

| Use case | OpenAI direct | OpenRouter | Notes |
|---|---|---|---|
| Inference (Sol) | `gpt-5.6-sol` | `~openai/gpt-sol-latest` → `openai/gpt-5.6-sol` | also `~openai/gpt-terra-latest`, `…-luna-latest`, `…-mini-latest`. `~` = latest-resolution alias; response `model` names the concrete one; unsupported efforts are remapped, not 400'd |
| STT | `gpt-transcribe` + `keywords` | `openai/gpt-transcribe` (~$0.0045/min, same) | **`prompt` is accepted but ignored** → our app-name vocabulary biasing is lost unless passed via `provider.options.<slug>`. 60 s upstream timeout, 25 MB, no streaming, no websocket. Alternatives: `deepgram/nova-3`, `whisper-large-v3-turbo` (~$0.0002/min), `nvidia/parakeet-tdt-0.6b-v3` |
| Streaming STT | `gpt-live-transcribe` (Realtime WS) | **none** | direct only |
| TTS | `gpt-4o-mini-tts` / `marin` | **OpenAI TTS is not in the live catalogue** (guide cites it; endpoint 404s) | via OpenRouter use `hexgrad/kokoro-82m` ($4/M chars), `google/gemini-3.1-flash-tts-preview`, `microsoft/mai-voice-2`, `deepgram/aura-2`, `mistralai/voxtral-mini-tts`. Default format is raw `pcm`; **sample rate undocumented** *(verify per model)*; `instructions` → `provider.options.openai.instructions`. No voices-list endpoint — voices table ships in the pack. |
| Images | — | ✅ 52 models | §4 |
| Video | — | ✅ | §4 |
| SVG | — | ✅ Recraft vector (generation only) | edit / animate / vectorize stay Quiver-only |
| Cutout · upscale · arbitrary endpoints | — | ✗ | fal-only |
| Jev | — | ✗ | TypeSafe key always required |

## 3. Core agent on OpenRouter

### Auth — "Sign in with OpenRouter" (PKCE)

1. Rust starts a one-shot loopback listener on `http://localhost:<random port>/cb` (explicitly supported; **custom URL schemes are not documented** — don't rely on `starkbot://`).
2. Open `https://openrouter.ai/auth?callback_url=…&code_challenge=…&code_challenge_method=S256&key_label=starkbot-neo` in the default browser.
3. `POST /api/v1/auth/keys {code, code_verifier, code_challenge_method}` → `{key}` (code single-use, 10 min) → Keychain. Fallback: omit `callback_url` and the user pastes the shown code.
4. The minted key is user-controlled and editable at `openrouter.ai/keys/<sha256hex(key)>`; **a spend limit cannot be set at mint time** → our own caps (03) remain the guard, and onboarding deep-links to that page suggesting a limit.

Status strip + Doctor use `GET /api/v1/key` (`usage`, `usage_daily`, `limit`, `limit_remaining`, `is_free_tier`). `/api/v1/credits` needs a management key — unusable here.

### BYOK
A user can attach **their own OpenAI key inside OpenRouter** (workspace BYOK): 5 % fee, waived under $25k/month; BYOK endpoints tried first; "shared capacity fallback: never" available. So "I have an OpenAI key but want one login / one bill / other model families" is a supported path — documented in onboarding help, nothing to build.

### Agent loop wiring

Two wire formats, both stateless:

| | Responses (beta) `/api/v1/responses` | Chat Completions `/api/v1/chat/completions` |
|---|---|---|
| State | `store:true` / `previous_response_id` → **400**; full history every turn (what metalcraft does anyway) | same |
| Reasoning replay | input `ReasoningItem` with `encrypted_content` (+ OpenRouter `signature`, `format`) next to `FunctionCallItem` / `FunctionCallOutputItem` — **schema-verified only**, the docs never show it with a tool call *(verify — this is spike S3b)* | assistant message carries `reasoning_details[]` (`reasoning.summary` · `reasoning.encrypted` · `reasoning.text`, each `id`/`format`/`index`); **pass the array back unmodified** on the message holding the `tool_calls` |
| Thought streaming | `response.reasoning_summary_text.delta` (prose guide says `response.reasoning.delta` — handle both) | `reasoning` deltas |
| Rust support | rig's OpenAI Responses client pointed at the OpenRouter base URL — *unverified* | **rig has an OpenRouter provider; chat-only; already round-trips `reasoning_details`** (0.37 tests show it) |

→ **Plan: OpenRouter goes through rig's OpenRouter chat provider.** metalcraft 0.12 generalises `ReasoningItem { id, encrypted }` into an opaque provider-tagged blob (`Responses{id, encrypted, summary}` | `ChatDetails(Vec<Value>)`) so both replay shapes survive `AgentState`. That is upstream item #6. `rig` is now 0.42 in the local metalcraft 0.12 tree; the OpenRouter-specific opaque replay shape remains unimplemented.

Request shape for every Sol call via OpenRouter:

```json
{ "model": "~openai/gpt-sol-latest",
  "provider": { "only": ["openai"], "allow_fallbacks": false, "data_collection": "deny" },
  "session_id": "<task uuid>",
  "reasoning": { "effort": "low" },
  "parallel_tool_calls": false }
```

- **`only: ["openai"]`, not `order`** — Sol is also served by Bedrock and Azure, and encrypted reasoning is only valid where it was minted (the luna-400 bug again). Setting `provider.order` *disables* sticky routing; `only` does not.
- **`session_id` = task id** → sticky routing → prompt-cache hits across the loop (automatic caching, 1,024-token minimum; `usage.prompt_tokens_details.cached_tokens`, `cache_discount`).
- Do **not** add `zdr: true` with `only:["openai"]` — the ZDR group appears to exclude first-party OpenAI endpoints → 503 *(verify)*.
- Endpoint tiers exist: `openai/flex` (50 % off, ~85 % uptime) and `openai/fast` (2× price); suffixes `:floor` / `:nitro` select them. Setting: *Speed* = standard | fast | economy. `:exacto` (tool-calling-quality sort) is irrelevant once pinned.
- Every response has exact `usage.cost` (`usage:{include:true}` is deprecated/no-op) → the spend meter is **exact** on this path, estimated on OpenAI-direct.
- Headers: `HTTP-Referer: https://starkbot.…`, `X-OpenRouter-Title: starkbot-neo`, `X-OpenRouter-Categories: cli-agent`; `X-OpenRouter-App-Visibility: hidden` until launch.
- Errors: 402 (credits, or the new *in-flight spending budget* for new/low-balance accounts — has `Retry-After` + `metadata.limit_source`), 429, 502 (upstream), 503 (no provider matches routing). **Mid-stream errors arrive as HTTP 200** with `finish_reason:"error"` + top-level `error` → the stream parser must treat that as a failed turn. Retry: honour `Retry-After`, exponential backoff, max 2.
- Privacy: prompts are **not logged by default** (opt-in only); `data_collection:"deny"` per request.
- Fees: 5.5 % on credit purchases (min $0.80), no inference markup. Latency overhead: "minimal", no number published → measured in S3b.
- Price discrepancy to resolve live: OpenRouter's catalogue lists Sol at **$2 / $10** per M (cache read $0.20; doubles above 272k prompt tokens) while my earlier read of OpenAI's page said $4 / $20. The price table therefore comes from **live APIs at startup** (`/api/v1/models` pricing fields), never hardcoded.

### Model picker
`GET /api/v1/models?supported_parameters=tools` → `id`, `alias_target`, `context_length`, `pricing`, `architecture.input/output_modalities`, `reasoning.supported_efforts`, `default_effort`. Inference picker on the OpenRouter path = `~*-latest` aliases first, then everything with tools + reasoning — which opens Claude / Gemini / etc. as the driver. (Jev's gates are model-agnostic, so nothing else changes.)

## 4. Media through OpenRouter (feeds 07)

### Images — `POST /api/v1/images`
`model`, `prompt`, `n` 1–10, `resolution` 512|1K|2K|4K, `aspect_ratio`, `size`, `quality`, `output_format` png|jpeg|webp|**svg**, `background` auto|**transparent**|opaque, `seed`, `stream`, `input_references[{type:"image_url", image_url:{url}}]` (https or data URL; ≤ 16, per-model cap), `provider.options.<slug>`. **No mask/inpainting** — edits are references + prompt. Response `data[].b64_json` + `media_type` + `usage.cost`. SSE previews (`image_generation.partial_image`) on OpenAI image models only → the Studio board can show them forming. Failed generation = 502, **not billed**.

Defaults the `neo-media` skill will recommend (prices live from `/images/models/{slug}/endpoints`):

| Job | Model | ~Price | Why |
|---|---|---|---|
| general still | `bytedance/seedream-4.5` | $0.04 | cheap, 4K, 14 refs, seed |
| best still / text in image | `openai/gpt-image-2.5-*` | ~$0.13 HQ | streams previews; **transparent bg** |
| multi-image edit / composite | `google/gemini-3.1-flash-image` | token-priced | 14 refs |
| fast drafts / shoot-outs | `black-forest-labs/flux.2-klein-4b` | $0.014/MP | `flux.2-pro` $0.03/MP for finals (FLUX.2 only — no Kontext) |
| transparent asset | `sourceful/riverflow-v2.5-fast` | ~$0.02 | transparent bg, `font_inputs` |
| **SVG logo / icon** | `recraft/recraft-v4.1-vector` | $0.08 | real `image/svg+xml`; `-pro-vector` $0.30; `-styles-vector` takes style refs |

"Cutout" gets a **partial** OpenRouter answer: regenerate/edit with `background:"transparent"` on a model that supports it — not true matting, so `media_cutout` stays fal-only and the skill explains the difference.

### Video — `POST /api/v1/videos`
Submit → 202 `{id, polling_url}` → `GET /videos/{id}` (`pending|in_progress|completed|failed|cancelled|expired`, `usage.cost`) → `GET /videos/{id}/content?index=0` **with the Bearer header**. `frame_images[{frame_type: first_frame|last_frame}]` = image-to-video; `generate_audio`. 30 s–minutes; poll ~10–30 s; **download immediately** (URL expiry undocumented). Webhooks are https-only → useless for a desktop app; poll. Not ZDR-eligible.

`kwaivgi/kling-v3.0-std` $0.084/s · `google/veo-3.1-lite` $0.03–0.08/s · `alibaba/wan-3.0` $0.05–0.20/s (≤ 30 s) · `google/veo-3.1` $0.20–0.60/s · `openai/sora-2-pro` $0.30–0.50/s · `bytedance/seedance-2.0`. Also listed: `black-forest-labs/flux-video-upscale`, `flux-video-edit`, `runway/aleph-2`.

### Audio cost accounting
TTS returns raw bytes → cost via `GET /api/v1/generation?id=<X-Generation-Id>` (`api_type` covers `tts|stt|video|image`). STT/image/video responses carry `usage.cost` inline.

### Moderation shapes to surface in the UI
403 + `error.metadata{reasons[], flagged_input, provider_name}` (chat) · 502 / in-stream `{"type":"error"}` (image) · `status:"failed"` + `error:"Content policy violation"` (video).

## 5. Design

### `neo-core::Provider` — per use case, not global

```rust
pub enum Provider { OpenAi, OpenRouter }
pub struct Routing { inference: (Provider, ModelRef), stt: (Provider, ModelRef), tts: Option<(Provider, ModelRef)> }
```

Either key, or both, may be stored. **Recommended hybrid when both exist:** inference → whichever the user prefers; STT → **OpenAI direct** (keeps `keywords` biasing and the streaming option); TTS → OpenAI direct (`gpt-4o-mini-tts`); media → OpenRouter. With *only* an OpenRouter key everything still works: STT `openai/gpt-transcribe` without biasing, TTS `kokoro-82m` (default) or Gemini/MAI voices.

### Onboarding step 2 (proposed)

> **Connect a model provider**
> **[ Sign in with OpenRouter ]** — one click, also unlocks image + video generation
> **[ Paste an OpenAI API key ]** — direct, lowest latency, best speech features
> *(you can add the other later)*

Then TypeSafe key, then permissions, as before.

### `MediaBackend` trait (in the `degen-media-maker` lib)

```rust
#[async_trait]
pub trait MediaBackend: Send + Sync {
    fn id(&self) -> &'static str;                         // "openrouter" | "fal" | "quiver"
    async fn capabilities(&self) -> Result<Capabilities>;  // from the live discovery APIs; cached 6 h
    async fn estimate(&self, job: &Job) -> Option<Usd>;
    async fn run(&self, job: Job, progress: &dyn Progress) -> Result<Vec<Artifact>>;
}
```

Router: explicit model id wins (`fal-ai/…`, `arrow-2`, `google/…`) → per-op user preference → first configured backend with the capability. Takes record `backend` + `model` + exact/estimated cost. Tools are capability-filtered at task start. OpenRouter base64 → straight into `takes/`.

| Op | openrouter | fal | quiver |
|---|---|---|---|
| still · edit · motion | ✅ | ✅ | — |
| vector (create) | ✅ Recraft | — | ✅ best |
| vectorize · vector_edit · animate | — | — | ✅ only |
| cutout · upscale · run-any-endpoint | — (video upscale only) | ✅ only | — |

Media enablement (07) → three options: **① OpenRouter** (already satisfied if the core signed in with it → one click + spend limits) · **② + fal.ai** · **③ + QuiverAI**.

## 6. Spikes added

- **S3b** — same gated-tool loop as S3 but via OpenRouter chat completions with `reasoning_details` replay, `only:["openai"]`, `session_id`: record added latency per call, cache-hit rate, and whether a 10-step tool loop survives without a reasoning-validation 400.
- **S6** — PKCE loopback sign-in from a Tauri build; `/api/v1/key` read-back.
- **S7** — one image (`seedream-4.5`), one SVG (`recraft-v4.1-vector`), one 4 s video (`veo-3.1-lite`), one TTS (`kokoro-82m`, detect pcm rate), one STT (`openai/gpt-transcribe`) — confirm shapes + `usage.cost`.

## 7. Knock-on edits

- 05: `neo-keys` accounts `openai | openrouter | typesafe | FAL_KEY | QUIVERAI_API_KEY`; registry merges both providers' model lists, tagged; price table from live APIs.
- 03: provider-aware model builder; 200-with-error stream handling; exact-cost path.
- 02: `Transcriber` / `Speaker` get OpenRouter impls (JSON base64 or multipart; pcm-rate probe).
- PLAN §6 metalcraft 0.12: + provider-tagged reasoning blob; the rig 0.37 → 0.42 bump is complete locally.

## Sources
openrouter.ai/docs: `api_reference/responses/overview` · `use-cases/reasoning-tokens` · `features/provider-routing` · `guides/routing/routers/latest-resolution` · `guides/routing/model-variants/overview` · `features/prompt-caching` · `guides/overview/auth/oauth` · `guides/overview/auth/byok` · `api_reference/limits` · `api_reference/errors-and-debugging` · `app-attribution` · `guides/features/zdr` · `guides/overview/multimodal/{image-generation,video-generation,stt,tts}` · `faq` — live JSON: `openrouter.ai/api/v1/{models,images/models,videos/models,providers}` — `openrouter.ai/works-with-openrouter/fal` · `fal.ai/models/openrouter/router` · `docs.quiver.ai/developers`
