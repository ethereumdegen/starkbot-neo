# 08 — Providers: direct vendor keys now, StarkRouter later

## Decision (K2)

- **Now: direct vendor keys.** The user brings their own key for each vendor; starkbot-neo talks to each vendor's API itself.
- **No OpenRouter** — not as a default, not as an option, not as a fallback. It cannot carry fal or Quiver, so it never reaches "one key", and it reroutes upstreams in ways Sol's reasoning replay cannot tolerate.
- **Later: StarkRouter** — the user's own gateway fronting **OpenAI + fal + Quiver** behind one key. It is a separate project; this doc only fixes the seam it plugs into and the list of things it must provide.
- Jev stays direct to TypeSafe unless StarkRouter later chooses to front it too.

## Keys today (K1)

| Key | Keychain account | Required | Asked for | Powers |
|---|---|---|---|---|
| OpenAI | `openai` | yes | onboarding step 2 | Sol, the text helper, `gpt-transcribe` / `gpt-live-transcribe`, `gpt-4o-mini-tts`. Never image generation (K4) |
| TypeSafe AI | `typesafe` | yes | onboarding step 3 | every Jev request, through `jev-nav::wire` |
| fal.ai | `FAL_KEY` | optional | media enablement flow | stills, edits, video, cutout, upscale, any fal endpoint |
| QuiverAI | `QUIVERAI_API_KEY` | optional | media enablement flow | SVG create / vectorize / edit / animate |
| pack keys | the `requires_env` name | optional | that pack's enablement flow | whatever the pack declares |

All in the macOS Keychain through `neo-keys` (05 §6): never in the webview, never entered by voice, never typed or read by a task.

## The seam: three traits in `neo-core::providers`

```rust
pub struct ProviderId(pub Cow<'static, str>);             // "openai" | "fal" | "quiver" | later "starkrouter" — open set
pub struct ModelRef   { pub provider: ProviderId, pub id: String }        // id may be symbolic: "sol-latest"
pub struct Endpoint   { pub base_url: Url, pub auth: Auth }               // the only way a client learns where to call and as whom
pub enum   Auth       { Bearer(Arc<Secret>), Header { name: HeaderName, prefix: &'static str, secret: Arc<Secret> } }
pub enum   Usd        { Exact(f64), Estimated(f64), Unpriced }            // Unpriced: units only (Jev tokens today)
pub struct Usage      { pub usd: Usd, pub units: Units }                  // tokens in/out/cached · audio s · chars · images · video s

#[async_trait]
pub trait InferenceProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn endpoint(&self) -> &Endpoint;                                      // neo-agent builds the rig Responses client from this
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>; // id, use case, capabilities?, price?
    async fn resolve(&self, r: &ModelRef) -> Result<ResolvedModel, ProviderError>;   // "sol-latest" → concrete id + caps + price
    fn usage(&self, m: &ResolvedModel, counts: &TokenCounts, reported_cost: Option<f64>) -> Usage;
    async fn key_info(&self) -> Result<Option<KeyInfo>, ProviderError>;   // usage / limit / remaining; None when the vendor has no such call
}

#[async_trait]
pub trait SpeechProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn capabilities(&self) -> SpeechCaps;                                 // batch_stt · streaming_stt · keywords · tts_pcm_stream
    fn transcriber(&self, m: &ResolvedModel, hints: SttHints) -> Result<Box<dyn Transcriber>, ProviderError>;
    fn speaker(&self, m: &ResolvedModel, voice: &VoiceRef) -> Result<Box<dyn Speaker>, ProviderError>;
}
#[async_trait] pub trait Transcriber: Send { async fn transcribe(&mut self, u: Utterance) -> Result<TranscriptStream, ProviderError>; }  // Partial* → Final + Usage
#[async_trait] pub trait Speaker: Send     { async fn speak(&mut self, text: &str, instructions: Option<&str>) -> Result<PcmStream, ProviderError>; } // 24 kHz s16le mono + Usage

#[async_trait]
pub trait MediaBackend: Send + Sync {                                     // defined in the degen-media-maker lib, re-exported by neo-media
    fn id(&self) -> &'static str;                                         // "fal" | "quiver" | later "starkrouter"
    async fn capabilities(&self) -> Result<Capabilities, MediaError>;     // job kinds + model catalog this backend can serve right now
    async fn schema(&self, endpoint: &str) -> Result<EndpointSchema, MediaError>;     // live parameter schema
    async fn estimate(&self, job: &Job) -> Result<Option<Usd>, MediaError>;           // shown in the action sentence before any spend
    async fn run(&self, job: Job, progress: &dyn Progress) -> Result<JobOutput, MediaError>;  // artifacts + Usage; queue position and SSE drafts via progress
}
```

`MediaBackend` lives upstream, so it cannot name `neo-core` types: its constructors take `(base_url, credential)` where the credential is a header-injecting closure, and `neo-media` adapts `Auth` → that closure and `JobOutput` usage → `Usage`. `jev-nav::wire::Client` is built the same way — `{ base_url, credential, model }` — which keeps `jev-nav` free of `neo-*` dependencies.

### Rules that keep the seam honest

1. **`base_url` + credential are injected.** Every HTTP/WS client is constructed from an `Endpoint` (or the closure form). No vendor host appears outside a provider impl — CI greps for it (05 §1 rule 3). Tests swap in a `wiremock` URL.
2. **Model ids are `ModelRef { provider, id }`, never bare strings** — in settings, in `tasks`, in traces. Symbolic ids (`sol-latest`) are stored as written and resolved by the provider.
3. **Cost flows through one `Usage`.** `Exact` only when the provider reported a dollar figure on that response; `Estimated` when we multiplied units by a fetched price; `Unpriced` when no price is known. Caps treat all three conservatively; the UI labels which it is.
4. **Capability filtering, not vendor checks.** Tools ask "does any configured backend serve `vector_edit`?", never "is there a Quiver key?". No fal key → no raster / video tools; no Quiver key → no vector tools; a router backend that serves everything lights them all up with no tool changes. The set is computed when a task starts, like the pack set.
5. **Key accounts are an open set** (05 §6). Adding `starkrouter` is one more account, not a schema change.
6. **One provider per use case, chosen in settings** — inference, text helper, STT, TTS, media each hold their own `ModelRef` / backend list. Direct keys and StarkRouter can coexist during migration.
7. **No provider-specific branches above the impls.** If `neo-agent`, `neo-voice` or `neo-media` needs `if provider == …`, the trait is missing a capability flag.

## What the single implementations do today

| Trait | Impl · home | Behaviour |
|---|---|---|
| `InferenceProvider` | `OpenAiInference` · `neo-agent::providers` | `GET /v1/models` → registry classification by id pattern (05 §7); resolves `sol-latest`; hands `Endpoint` to the rig OpenAI **Responses** client that metalcraft drives (`store: false`, `include: ["reasoning.encrypted_content"]`, reasoning summaries, streaming); also serves the text helper (`gpt-5.6-luna`, reasoning off). `usage()` → `Estimated` from the fetched price table. `key_info()` → `None`. Doubles as the `openai` `KeyValidator`. |
| `SpeechProvider` | `OpenAiSpeech` · `neo-voice::providers` | batch STT: multipart `POST /v1/audio/transcriptions` with `prompt` + `keywords`; streaming STT: Realtime transcription WebSocket with local VAD as the authority; TTS: `POST /v1/audio/speech`, `response_format: pcm`, chunked. Usage = audio seconds / characters → `Estimated`. |
| `MediaBackend` | `Fal`, `Quiver` · `degen-media-maker` lib | `Fal`: queue submit → follow the returned `status_url` → result; per-endpoint schema and price discovery; `Estimated` from fal's price. `Quiver`: generate / vectorize / edit / animate with SSE drafts forwarded to `progress`; token-priced estimate. Each validates its own key (401 = invalid). |

## StarkRouter wish-list — what it must provide before starkbot-neo switches to one key

**Inference**
1. OpenAI-compatible **Responses API** with **faithful `reasoning.encrypted_content` replay**: items returned by an upstream are accepted back byte-for-byte on the next turn, and a conversation is **never rerouted to a different upstream mid-conversation** (a rerouted replay is a hard 400 — the luna lesson). Pinning is per conversation; failover only at conversation start.
2. **Reasoning summaries** (`reasoning.summary: "auto"`) passed through, and **streaming** of output-text and reasoning-summary deltas with OpenAI's event names.
3. Function calling, `parallel_tool_calls: false`, image input, image parts in tool results, prompt-cache behaviour and cached-token counts passed through untouched.

**Speech**
4. `/audio/transcriptions` that **honours `prompt` and `keywords`** — forwarded, not dropped — plus **streaming STT** (the Realtime transcription session, WebSocket passthrough).
5. `/audio/speech` with **`pcm` streaming** (24 kHz s16le, chunked, first byte < 300 ms added latency), `voice` and `instructions` passed through.

**Media**
6. **fal queue passthrough** for any endpoint id: submit / status / result / cancel with fal's semantics, plus **per-endpoint schema and price discovery**.
7. **Quiver passthrough**: generate / vectorize / edit / animate, **SSE drafts preserved** event by event.

**Catalog and money**
8. `GET /models` with **capabilities** (use case, modalities, reasoning, context), **live prices**, deprecation flags, and a **`sol-latest` alias** resolved server-side (the response names the concrete model).
9. **Exact `usage.cost` in USD on every response** — inference, speech, media, streamed or not.
10. A **key-info endpoint**: label, usage today / this month, limit, remaining, expiry.
11. **Per-key spend limits set at creation** (daily and total), enforced server-side with a distinct error code — the backstop behind K5.

**Sign-in and privacy**
12. **OAuth PKCE sign-in from a desktop app with a `127.0.0.1` loopback callback** → a minted, revocable, per-device key. Nobody pastes anything.
13. **No prompt logging by default**; metadata-only accounting; any debugging retention is opt-in per key.

Until items 1, 4, 5 and 9 are demonstrably true (a Rust conformance suite, `neo dev router-conformance`, replays our recorded sessions against it), direct keys remain the default.

## How onboarding collapses

| | Today | With StarkRouter |
|---|---|---|
| Onboarding | Welcome → OpenAI key → TypeSafe key → Microphone → Accessibility → listening statement | Welcome → **Sign in with StarkRouter** (browser, PKCE, loopback) → TypeSafe key *(gone too if StarkRouter fronts Jev)* → Microphone → Accessibility → listening statement |
| Media enablement | consent → fal key → Quiver key → spend limits | consent → spend limits (**one click**; limits also pushed to the key, item 11) |
| Spend meter | `Estimated` | `Exact` |
| Model registry | id patterns + fetched `prices.json` | capabilities and prices from `/models`; `sol-latest` server-side |
| Code change | — | one `StarkRouter` struct implementing the three traits, one `starkrouter` key account, settings defaults flipped. No tool, schema or UI-flow change. |

Direct vendor keys stay supported afterwards as an advanced option in Settings → Keys.

## Design input

[research/openrouter-2026-09.md](research/openrouter-2026-09.md) records what OpenRouter gets right (PKCE sign-in with a loopback callback, `usage.cost`, a key-info call, `/models` with pricing, `…-latest` aliases) and where it falls short for us (no fal, no real Quiver coverage, no spend limit at key mint, the STT `prompt` accepted but ignored, encrypted reasoning valid only on the upstream that minted it). It is **design input for StarkRouter only** — nothing in starkbot-neo integrates with OpenRouter.
