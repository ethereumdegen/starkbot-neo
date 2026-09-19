# 08 — Providers: ChatGPT subscription or direct keys now, StarkRouter later

## Decision (K2)

- **Inference now has two first-class paths:** **Connect ChatGPT** through OpenAI's official Codex app-server (uses the user's eligible ChatGPT plan allowance), or bring an OpenAI API key (usage-based billing). The user chooses; both may be configured.
- **The subscription path is Codex inference only.** It can power Sol, questions and the text helper. It does not turn a ChatGPT plan into a general API credential: STT, streaming STT, TTS, fal, Quiver and Jev still need their own provider/key.
- **No home-grown ChatGPT OAuth and no token borrowing.** Neo never reads `~/.codex/auth.json`, imports OMP credentials, asks for an OAuth token, or calls undocumented ChatGPT endpoints. It drives the documented `codex app-server` auth/account and thread APIs; Codex owns browser login, refresh and credential storage.
- **No OpenRouter** — not as a default, option or fallback. It cannot carry fal or Quiver, and rerouting breaks reasoning replay.
- **Later: StarkRouter** — the user's own gateway fronting **OpenAI API + fal + Quiver** behind one key. It is separate from ChatGPT subscription access.
- Jev stays direct to TypeSafe unless StarkRouter later chooses to front it too.

## Credentials today (K1)

| Credential | Storage owner | Required | Asked for | Powers |
|---|---|---|---|---|
| ChatGPT account | official Codex app-server; tokens in its dedicated macOS Keychain entry | one of ChatGPT or OpenAI key for inference | onboarding inference choice | Sol, questions, text helper through Codex models; plan allowance/rate limits, not API billing |
| OpenAI API key | `neo-keys` account `openai` | alternative for inference; required for OpenAI speech | onboarding inference choice and Voice settings when needed | Responses inference, `gpt-transcribe` / `gpt-live-transcribe`, `gpt-4o-mini-tts`; never image generation (K4) |
| TypeSafe AI | `neo-keys` account `typesafe` | yes | onboarding | every Jev request, through `jev-nav::wire` |
| fal.ai | `neo-keys` account `FAL_KEY` | optional | media enablement flow | stills, edits, video, cutout, upscale, any fal endpoint |
| QuiverAI | `neo-keys` account `QUIVERAI_API_KEY` | optional | media enablement flow | SVG create / vectorize / edit / animate |
| pack keys | `neo-keys` account = `requires_env` name | optional | that pack's enablement flow | whatever the pack declares |

Direct keys use `neo-keys`: never in the webview, never entered by voice, never typed or read by a task. ChatGPT credentials are different: the official Codex helper owns them, uses Keychain storage, and exposes only redacted account/plan/rate-limit state over local stdio JSON-RPC. Neo never receives an access or refresh token.

## The seams: provider traits plus an agent runtime

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

```rust
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn id(&self) -> &ProviderId;                                          // "openai" | "chatgpt-codex"
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    async fn run(&self, request: AgentRequest, tools: &dyn ToolDispatcher, events: &dyn AgentEvents)
        -> Result<AgentOutcome, ProviderError>;
    async fn complete_json(&self, request: TextRequest, schema: Value)
        -> Result<TextOutcome, ProviderError>;                            // text helper / extraction path
    async fn allowance(&self) -> Result<Option<Allowance>, ProviderError>; // API price/key info or ChatGPT rate windows
}
```

`MetalcraftRuntime<OpenAiInference>` implements this with rig/Responses and owns the existing Sol tool loop. `CodexRuntime` implements it with app-server threads and client-executed dynamic tools. `neo-agent` depends only on `AgentRuntime`; it does not branch on OpenAI-key versus ChatGPT auth. `InferenceProvider` remains the raw HTTP seam used by the metalcraft runtime and future StarkRouter.

`MediaBackend` lives upstream, so it cannot name `neo-core` types: its constructors take `(base_url, credential)` where the credential is a header-injecting closure, and `neo-media` adapts `Auth` → that closure and `JobOutput` usage → `Usage`. `jev-nav::wire::Client` is built the same way — `{ base_url, credential, model }` — which keeps `jev-nav` free of `neo-*` dependencies.

### Rules that keep the seam honest

1. **`base_url` + credential are injected.** Every HTTP/WS client is constructed from an `Endpoint` (or the closure form). No vendor host appears outside a provider impl — CI greps for it (05 §1 rule 3). Tests swap in a `wiremock` URL.
2. **Model ids are `ModelRef { provider, id }`, never bare strings** — in settings, in `tasks`, in traces. Symbolic ids (`sol-latest`) are stored as written and resolved by the provider.
3. **Cost flows through one `Usage`.** `Exact` only when the provider reported a dollar figure on that response; `Estimated` when we multiplied units by a fetched price; `Unpriced` when no price is known. Caps treat all three conservatively; the UI labels which it is.
4. **Capability filtering, not vendor checks.** Tools ask "does any configured backend serve `vector_edit`?", never "is there a Quiver key?". No fal key → no raster / video tools; no Quiver key → no vector tools; a router backend that serves everything lights them all up with no tool changes. The set is computed when a task starts, like the pack set.
5. **Key accounts are an open set** (05 §6). Adding `starkrouter` is one more account, not a schema change.
6. **One provider per use case, chosen in settings** — Sol/inference and text helper select an `AgentRuntime` + model; STT and TTS select a `SpeechProvider`; media selects backend preferences. ChatGPT, direct keys and StarkRouter can coexist, but a task snapshots one runtime and never switches mid-task.
7. **No provider-specific branches above the impls.** If `neo-agent`, `neo-voice` or `neo-media` needs `if provider == …`, the trait/runtime is missing a capability flag. A rate-limit or outage never silently falls through from ChatGPT allowance to a billable API key.

## Implementations

| Seam | Impl · home | Behaviour |
|---|---|---|
| `AgentRuntime` | `MetalcraftRuntime<OpenAiInference>` · `neo-agent::providers` | `GET /v1/models` → registry classification; resolves `sol-latest`; rig OpenAI Responses client (`store: false`, encrypted reasoning replay, summaries, streaming) runs the Sol loop and strict-schema text helper. Usage is estimated from the signed price table. |
| `AgentRuntime` | `CodexRuntime` · `neo-agent::providers::codex` | supervises the bundled official Codex app-server over stdio JSONL; ChatGPT-managed auth; one Codex thread per neo task; Starkbot tools are app-server dynamic tools whose calls return through `ToolDispatcher`/`Gated<T>`; strict `outputSchema` serves helper calls; allowance comes from `account/rateLimits/read`. |
| `InferenceProvider` | `OpenAiInference` · `neo-agent::providers` | raw OpenAI Responses endpoint/model catalog used by `MetalcraftRuntime`; also the `openai` API-key validator. |
| `SpeechProvider` | `OpenAiSpeech` · `neo-voice::providers` | batch STT, Realtime streaming STT and streaming PCM TTS. **API key only; ChatGPT/Codex auth is never used for audio endpoints.** |
| `MediaBackend` | `Fal`, `Quiver` · `degen-media-maker` lib | fal queue and Quiver SSE implementations, each with its own key validation and estimated usage. |

## Connect ChatGPT — official Codex app-server adapter

### Supported integration surface

Neo ships a pinned build of OpenAI's Apache-2.0 Codex binary as a signed nested helper and launches `codex app-server --listen stdio://`. This is the documented custom-client surface; direct use of the private ChatGPT/Codex HTTP protocol is forbidden. CI records the upstream version, source commit, archive SHA-256, license and generated JSON schema. Updating Codex is an explicit dependency PR that runs the provider conformance suite.
The public repo commits `third_party/codex/manifest.toml`, the upstream licence/notice and the generated protocol schema — **not** a platform binary. `neo dev fetch-codex` downloads the manifest's official universal artifact (or the two pinned architecture artifacts and combines them), verifies SHA-256 before it enters a gitignored vendor cache, and never runs an unverified byte. Local builds without that cache compile and run with `chatgpt-codex` reported unavailable; release CI fetches, verifies, signs and bundles it. End users install nothing separately.

The helper gets a dedicated `CODEX_HOME` under Application Support and `cli_auth_credentials_store = "keyring"`. It does not reuse or mutate the user's normal `~/.codex` session. The app initializes with client name `starkbot_neo`; before claiming Enterprise support, register that client identity with OpenAI as its app-server guidance requests.

### Login and account lifecycle

1. Settings/Onboarding → **Connect ChatGPT** starts app-server and sends `account/login/start {type:"chatgpt"}`.
2. Neo opens the returned `authUrl` in the user's default browser. Codex owns the loopback callback, exchanges the code, stores/refreshes tokens and emits `account/login/completed` plus `account/updated`.
3. Neo stores only `{connected, email?, plan_type?, workspace?, updated_at}` status in SQLite. No token crosses the JSON-RPC boundary.
4. `account/read` checks state; `account/logout` disconnects this app's dedicated session. Device-code login is an accessibility fallback.
5. `account/rateLimits/read` and update notifications drive a plan/allowance card with used percentage and reset time. Reaching the limit pauses new Sol work and offers **wait**, **switch to API key**, or **change model**; switching to a billable key is explicit.

The screen says plainly: content sent through this path follows the user's ChatGPT workspace controls and data settings, not API-platform retention settings. Business/Enterprise workspace restrictions and model availability are authoritative.

### Tool and process boundary

At `thread/start`, `CodexRuntime` registers the task's snapshotted Starkbot tools as experimental `dynamicTools`. An `item/tool/call` request is schema-validated, dispatched through the same `ToolDispatcher` and `Gated<T>` as metalcraft, then answered with bounded content items. Dynamic tools never gain a second implementation of browser, AX, media, canvas, pack or confirm logic.

Codex is coding-oriented and normally has local tools; those are not an acceptable Starkbot surface. Every thread therefore uses a dedicated empty working directory, `approvalPolicy: "never"`, a `readOnly` sandbox with restricted read roots, and no command-network access. The system instruction permits only the supplied dynamic tools. Any `commandExecution`, `fileChange`, MCP/app call, permission request or unknown effect item causes immediate `turn/interrupt` and `ProviderError::Protocol`; neo never approves it. Only agent messages, reasoning summaries and dynamic-tool calls are accepted.

`dynamicTools` is currently an app-server experimental API. The ChatGPT runtime is therefore badged **Beta** until OpenAI stabilizes it. The pinned-version conformance test must prove login, refresh, model listing, strict output schema, each tool-result content kind, cancellation, allowance reporting and the forbidden-item interrupt. Failure disables only `chatgpt-codex`; an API-key runtime remains available if configured.

### Limits and accounting

- ChatGPT allowance is not an OpenAI API dollar balance. Calls record tokens when reported plus `Usd::Unpriced`; the UI shows plan rate windows, not a fictitious dollar cost.
- Dollar caps cannot constrain included-plan usage. Step, decision, wall, tool and media caps still apply; ChatGPT rate-limit exhaustion is a separate hard stop.
- Only models returned/accepted by the connected Codex account are selectable. No mapping claims that every OpenAI API model exists in ChatGPT.
- ChatGPT auth cannot call speech, image, fal, Quiver, TypeSafe or pack endpoints. Keys for those remain separate.
- No automatic fallback between subscription and API billing within a task or after a 429. The user starts a new task after explicitly choosing another runtime.

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

Until items 1, 4, 5 and 9 are demonstrably true, the direct OpenAI API runtime remains available. StarkRouter may replace that API-key path; it never impersonates or replaces the separate ChatGPT/Codex runtime.

## How onboarding collapses

| | Today | With StarkRouter |
|---|---|---|
| Onboarding | Welcome → **Connect ChatGPT or use OpenAI API key** → TypeSafe key → optional OpenAI speech key if not already supplied → Microphone/typed-only choice → Accessibility → listening statement | Welcome → **Connect ChatGPT, use OpenAI API key, or sign in with StarkRouter** → TypeSafe key *(gone too if StarkRouter fronts Jev)* → speech/typed-only choice → Microphone → Accessibility → listening statement |
| Media enablement | consent → fal key → Quiver key → spend limits | consent → spend limits (**one click**; limits also pushed to the key, item 11) |
| Spend meter | `Estimated` | `Exact` |
| Model registry | id patterns + fetched `prices.json` | capabilities and prices from `/models`; `sol-latest` server-side |
| Code change | — | one `StarkRouter` `InferenceProvider` wrapped by `MetalcraftRuntime`, one `starkrouter` key account, settings option added. No tool/schema change; the ChatGPT runtime remains independent. |

Direct vendor keys and Connect ChatGPT stay supported after StarkRouter ships.

## Design input

[research/openrouter-2026-09.md](research/openrouter-2026-09.md) records what OpenRouter gets right (PKCE sign-in with a loopback callback, `usage.cost`, a key-info call, `/models` with pricing, `…-latest` aliases) and where it falls short for us (no fal, no real Quiver coverage, no spend limit at key mint, the STT `prompt` accepted but ignored, encrypted reasoning valid only on the upstream that minted it). It is **design input for StarkRouter only** — nothing in starkbot-neo integrates with OpenRouter.
