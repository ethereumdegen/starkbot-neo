# 08 — Providers: API keys or vendor subscriptions (OpenAI, Anthropic) now, StarkRouter later

> **2026-09-19 (K6):** Starkbot needs exactly two kinds of credential: a **TypeSafe AI key** (Jev, always) and **one inference connection**. Speech stays OpenAI-API-key only. fal and QuiverAI keys are not Starkbot credentials: the media apps it operates (Diffusion Studio and Degen Media Studio) own their generation keys and credits (K1′, K4); see [12-media-apps](12-media-apps.md).
>
> **2026-09-19d (K7 · A25 — supersedes the token-custody half of K6/A18):** the two **subscription** connections are now **OAuth credentials Starkbot owns**: it runs the PKCE login itself, keeps the refresh and access tokens in the macOS Keychain through `neo-keys` (accounts `anthropic-oauth`, `openai-codex`), refreshes them in-process and calls **Anthropic Messages** and the **Codex Responses backend** directly. The pinned Codex app-server and Claude Code CLI stay as **fallbacks** for a user who prefers not to hand Starkbot a token. A subscription token therefore *does* reach Starkbot — it reaches `neo-keys` and the provider impl, and nothing else.

## Decision (K7 — token custody; K6's "subscription paths are inference only" and speech rule stand)

- **Inference has six paths:** two **API keys** — an **OpenAI API key** and an **Anthropic API key** (usage-based billing) · two **subscription OAuth** connections Starkbot owns end to end — a **Claude Pro/Max** plan against Anthropic Messages and a **ChatGPT Plus/Pro (Codex)** plan against the Codex Responses backend (plan allowance) · two **vendor-CLI fallbacks** — the pinned official Codex app-server and the pinned official Claude Code CLI, for a user who prefers not to hand Starkbot a token. Exactly one is required at onboarding; more than one may be configured, and a task snapshots one runtime and never switches mid-task.
- **Starkbot holds the subscription credential (K7).** It performs the PKCE (S256) browser login itself on a loopback callback, receives the code on that callback (or from a pasted redirect URL), exchanges it, stores the refresh and access tokens in the macOS Keychain through `neo-keys` as one JSON blob per provider, refreshes them in-process when the access token is within five minutes of expiry, and sends the access token on each request. No token reaches SQLite, settings, an `AppEvent`, a trace, a log, a diagnostic bundle, a snapshot test, the TUI or the webview; only the redacted `provider_accounts` row is persisted. This is the mechanism the user's own OMP harness uses; it buys streaming and the full Messages/Responses surface and removes the per-turn cost of spawning a vendor CLI. The cost is stated plainly: Starkbot is a token custodian now.
- **Every subscription path is inference only.** Any of them can power Sol, questions and the text helper. None turns a consumer plan into a general API credential: Jev and user-installed pack integrations still need their own credentials, while media generation remains inside the media apps and their accounts (K1′, K4).
- **Speech is OpenAI-API-key only.** `gpt-transcribe`, the optional `gpt-live-transcribe` and `gpt-4o-mini-tts` are reached with an OpenAI API key and with nothing else; no subscription path and no Anthropic credential carries speech. A subscription-only user starts typed-only until an OpenAI key is added.
- **No token borrowing and no invented protocol.** Starkbot never reads another program's credential file, never imports OMP's stored credentials, and never spends one connection's token on another surface. What it does speak is each vendor's own client OAuth and inference surface — the same client id, scopes, callback and headers that vendor's first-party client sends. Each vendor's terms on third-party use of a consumer-account login are the user's to accept *(verify both before public release)*.
- **No OpenRouter** — not as a default, option or fallback. Rerouting breaks reasoning replay and it cannot provide the required end-to-end runtime contract.
- **Later: StarkRouter** — the user's own OpenAI-compatible inference gateway behind one key. It is separate from the subscription paths and from media-app credentials.
- Jev stays direct to TypeSafe unless StarkRouter later chooses to front it too.

## Credentials today (K6 · K7)

| Credential | Storage owner | Required | Asked for | Powers |
|---|---|---|---|---|
| OpenAI API key | `neo-keys` account `openai` | one of the six inference connections; **always required for speech** | onboarding inference choice and Voice settings when needed | Responses inference, `gpt-transcribe` / `gpt-live-transcribe`, `gpt-4o-mini-tts`; never image generation (K4) |
| Anthropic API key | `neo-keys` account `anthropic` | one of the six inference connections | onboarding inference choice | Anthropic Messages inference; no speech and no media generation on this credential (K1′, K4) |
| **Claude Pro/Max OAuth credential** (K7) | **`neo-keys` account `anthropic-oauth`** — one JSON blob `{access_token, refresh_token, expires_at_ms, account_id?, email?, plan?}`, refreshed in-process | one of the six inference connections | onboarding inference choice → **Connect Claude** | Sol, questions, text helper on Claude models through `https://api.anthropic.com/v1`; plan rate windows (5 h / 7 d), not API billing |
| **ChatGPT Plus/Pro OAuth credential** (K7) | **`neo-keys` account `openai-codex`** — the same blob shape; `account_id` is the `chatgpt_account_id` claim of the `id_token` | one of the six inference connections | onboarding inference choice → **Connect ChatGPT** | Sol, questions, text helper on Codex models through `https://chatgpt.com/backend-api`; plan allowance, not API billing |
| ChatGPT account through the Codex app-server *(fallback)* | official Codex helper; tokens in its dedicated `CODEX_HOME` keyring entry | alternative to the ChatGPT OAuth path | Settings → Connections → "drive the vendor CLI instead" | the same Codex models, through the helper's stdio protocol |
| Claude subscription through the Claude Code CLI *(fallback)* | pinned official Claude Code helper, in its own dedicated `CLAUDE_CONFIG_DIR` plus the Keychain entry keyed to that directory | alternative to the Claude OAuth path | Settings → Connections → "drive the vendor CLI instead" | the same Claude models, through the CLI's stream-JSON protocol |
| TypeSafe AI | `neo-keys` account `typesafe` | yes | onboarding | every Jev request, through `jev-nav::wire` |
| pack keys | `neo-keys` account = `requires_env` name | optional | that pack's enablement flow | whatever the pack declares |

Every Starkbot-held credential — the API keys and the two OAuth blobs — lives in `neo-keys` and nowhere else: never in the webview, never in the TUI, never entered by voice, never typed or read by a task, never in SQLite, never in an event, log or diagnostic. The OAuth blobs differ from the key accounts only in shape (JSON with an expiry rather than a bare key) and in lifecycle (refreshed in-process before use); the same `Secret` redaction and the same clippy fence on `expose()` apply. Only the redacted `provider_accounts` row — `{status, email?, plan_type?, workspace?, allowance?, updated_at}` — is persisted, and an `id_token` is parsed for the account id and email only, never trusted for authorization. The two CLI fallbacks keep the older shape: the pinned helper owns its credential in its own home — Codex in its `CODEX_HOME` with keyring storage, Claude Code in its `CLAUDE_CONFIG_DIR` — neither reuses the user's everyday session, and no token crosses that stdio boundary.

## The seams: provider traits plus an agent runtime

```rust
pub struct ProviderId(pub Cow<'static, str>);             // "openai" | "anthropic" | "anthropic-oauth" | "openai-codex" | fallbacks "chatgpt-codex", "claude-subscription" | later "starkrouter" — open set
pub struct ModelRef   { pub provider: ProviderId, pub id: String }        // id may be symbolic: "sol-latest"
pub struct Endpoint   { pub base_url: Url, pub auth: Auth }               // the only way a client learns where to call and as whom
pub enum   Auth       { Bearer(Arc<Secret>), Header { name: HeaderName, prefix: &'static str, secret: Arc<Secret> } }
pub enum   Usd        { Exact(f64), Estimated(f64), Unpriced }            // Unpriced: units only (Jev and subscription-plan usage)
pub struct Usage      { pub usd: Usd, pub units: Units }                  // tokens in/out/cached · audio seconds · characters

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

```

```rust
#[async_trait]
pub trait AgentRuntime: Send + Sync {
    fn id(&self) -> &ProviderId;                                          // "openai" | "anthropic" | "anthropic-oauth" | "openai-codex" | "chatgpt-codex" | "claude-subscription"
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
    async fn run(&self, request: AgentRequest, tools: &dyn ToolDispatcher, events: &dyn AgentEvents)
        -> Result<AgentOutcome, ProviderError>;
    async fn complete_json(&self, request: TextRequest, schema: Value)
        -> Result<TextOutcome, ProviderError>;                            // text helper / extraction path
    async fn allowance(&self) -> Result<Option<Allowance>, ProviderError>; // API price/key info or subscription rate windows
}
```

`MetalcraftRuntime<OpenAiInference>` and `MetalcraftRuntime<AnthropicInference>` implement this with rig — OpenAI Responses and Anthropic Messages on an API key — and own the existing Sol tool loop. `MetalcraftRuntime<AnthropicOauthInference>` and `MetalcraftRuntime<CodexOauthInference>` run the same loop on a subscription OAuth credential (A25): inference goes through **metalcraft** (rig) wherever it can carry the request, and the provider impl contributes only the base URL, the vendor header set and the access token it asks `OauthStore` for on each request. `CodexRuntime` (app-server threads and client-executed dynamic tools) and `ClaudeCodeRuntime` (headless streaming-JSON sessions whose only tools arrive over a local MCP server) remain as the vendor-CLI fallbacks. All six dispatch the same snapshotted Starkbot tools through the same `ToolDispatcher` and `Gated<T>` (A22, A25). `neo-agent` depends only on `AgentRuntime`; it does not branch on API-key versus OAuth versus helper auth, nor on which vendor sits behind any of them. `InferenceProvider` remains the raw HTTP seam used by the metalcraft runtimes and future StarkRouter; an OAuth runtime reaches it as `Auth::Bearer` over a token fetched per request and cached nowhere but `OauthStore`.

`jev-nav::wire::Client` follows the same injection rule—`{ base_url, credential, model }`—which keeps `jev-nav` free of `neo-*` dependencies. Media-provider seams belong to Degen Media Studio, not Starkbot (12, 13).

### Rules that keep the seam honest

1. **`base_url` + credential are injected.** Every HTTP/WS client is constructed from an `Endpoint` (or the closure form). No vendor host appears outside a provider impl — CI greps for it (05 §1 rule 3). Tests swap in a `wiremock` URL.
2. **Model ids are `ModelRef { provider, id }`, never bare strings** — in settings, in `tasks`, in traces. Symbolic ids (`sol-latest`) are stored as written and resolved by the provider.
3. **Cost flows through one `Usage`.** `Exact` only when the provider reported a dollar figure on that response; `Estimated` when we multiplied units by a fetched price; `Unpriced` when no price is known. Caps treat all three conservatively; the UI labels which it is.
4. **Capability filtering, not vendor checks.** Callers ask whether the selected runtime supports reasoning, tools, strict JSON, image input, streaming or a speech capability; they never branch on a provider id.
5. **Key accounts are an open set** (05 §6). Adding `starkrouter` or a user-installed pack credential is not a schema change.
6. **One provider per use case, chosen in settings** — Sol/inference and text helper select an `AgentRuntime` + model; STT and TTS select a `SpeechProvider`. All six inference connections and StarkRouter can coexist, but a task snapshots one runtime and never switches mid-task.
7. **No provider-specific branches above the impls.** If `neo-agent` or `neo-voice` needs `if provider == …`, the trait/runtime is missing a capability flag. A rate-limit or outage never silently falls through from a subscription allowance to a billable API key.

## Implementations

| Seam | Impl · home | Behaviour |
|---|---|---|
| `AgentRuntime` | `MetalcraftRuntime<OpenAiInference>` · `neo-agent::providers` | `GET /v1/models` → registry classification; resolves `sol-latest`; rig OpenAI Responses client (`store: false`, encrypted reasoning replay, summaries, streaming) runs the Sol loop and strict-schema text helper. Usage is estimated from the signed price table. |
| `AgentRuntime` | `MetalcraftRuntime<AnthropicInference>` · `neo-agent::providers` | model list → registry classification *(verify the Anthropic model-list shape and pagination)*; resolves `sol-latest`; rig Anthropic Messages client (streaming, extended thinking whose signed thinking blocks are replayed byte-for-byte on the next turn) runs the same Sol loop; strict-schema helper calls go through one required tool whose input schema is the target schema *(verify whether a first-class structured-output field is available on the pinned API version)*. Usage is estimated from the signed price table. |
| `AgentRuntime` | `MetalcraftRuntime<AnthropicOauthInference>` · `neo-agent::providers::oauth` | **Claude Pro/Max on the credential Starkbot holds (K7).** Same rig Anthropic Messages loop as the API-key runtime, pointed at `https://api.anthropic.com/v1` with `Authorization: Bearer <access>` (never `x-api-key`), `anthropic-version: 2023-06-01`, the `claude-code-20250219,oauth-2025-04-20,…` beta list and the Claude Code system instruction as the first system block — the header and system shape the vendor's own client sends. Access token per request from `OauthStore`, refreshed 5 min before expiry. Allowance is the plan's 5-hour and 7-day windows; `Usd::Unpriced`. |
| `AgentRuntime` | `MetalcraftRuntime<CodexOauthInference>` · `neo-agent::providers::oauth` | **ChatGPT Plus/Pro on the credential Starkbot holds (K7).** Codex Responses at `https://chatgpt.com/backend-api/codex/responses` with `Authorization: Bearer <access>`, `ChatGPT-Account-Id: <id_token account id>`, `originator`, `session_id` and `OpenAI-Beta: responses=experimental`; streaming and encrypted reasoning replay as on the API path *(verify which of these headers this surface rejects for a non-Codex originator)*; strict-schema helper calls use the Responses schema field *(verify its availability on this surface)*. Allowance is the plan window reported by the backend; `Usd::Unpriced`. |
| `AgentRuntime` *(fallback)* | `CodexRuntime` · `neo-agent::providers::codex` | supervises the bundled official Codex app-server over stdio JSONL; ChatGPT-managed auth; one Codex thread per neo task; Starkbot tools are app-server dynamic tools whose calls return through `ToolDispatcher`/`Gated<T>`; strict `outputSchema` serves helper calls; allowance comes from `account/rateLimits/read`. Offered to users who will not hand Starkbot a token (A25). |
| `AgentRuntime` *(fallback)* | `ClaudeCodeRuntime` · `neo-agent::providers::claude_code` | supervises the pinned official Claude Code CLI in headless streaming mode over stdio NDJSON (`--print --output-format stream-json --input-format stream-json --verbose`) *(verify)*; subscription-managed auth inside its own `CLAUDE_CONFIG_DIR`; one CLI session per neo task (`--session-id <uuid>`, resumed for later turns); Starkbot tools are supplied as one local stdio **MCP server** (`--mcp-config` made exclusive with `--strict-mcp-config`) whose calls return through `ToolDispatcher`/`Gated<T>`; `--json-schema` serves strict helper calls *(verify)*; allowance comes from the CLI's own plan reporting — rolling five-hour and weekly windows, never dollars. Offered to users who will not hand Starkbot a token (A25). |
| `InferenceProvider` | `OpenAiInference` · `neo-agent::providers` | raw OpenAI Responses endpoint/model catalog used by `MetalcraftRuntime`; also the `openai` API-key validator. |
| `InferenceProvider` | `AnthropicInference` · `neo-agent::providers` | raw Anthropic Messages endpoint/model catalog used by `MetalcraftRuntime`; also the `anthropic` API-key validator. |
| `InferenceProvider` | `AnthropicOauthInference` · `CodexOauthInference` · `neo-agent::providers::oauth` | the same seam over an `OauthStore` + `OauthClient` pair: `endpoint()` returns the injected base URL with `Auth::Bearer` over the freshly refreshed access token, plus the vendor header set. Neither caches a token of its own; neither names a host outside this module (rule 1). |
| `SpeechProvider` | `OpenAiSpeech` · `neo-voice::providers` | batch STT, Realtime streaming STT and streaming PCM TTS. **API key only; no subscription credential — OAuth or helper-held — is ever used for audio endpoints, and there is no Anthropic speech path (K6).** |

## Connect ChatGPT — subscription OAuth (primary), Codex app-server (fallback)

### Subscription OAuth adapter — `MetalcraftRuntime<CodexOauthInference>` (K7 · A25)

Starkbot runs this login itself and holds the credential. Every wire fact below is grep-confirmable in the user's OMP bundle (`~/.bun/install/global/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js`, auth provider id `openai-codex`); anything that is not is marked *(verify)*.

| Step | Wire fact |
|---|---|
| Client id | `app_EMoamEEZ73f0CkXaXp7hrann` |
| Authorize | `https://auth.openai.com/oauth/authorize` — PKCE `code_challenge_method=S256`, hex `state`, standard authorize params, plus `id_token_add_organizations=true`, `codex_cli_simplified_flow=true` and `originator=<ours>` (OMP sends `omp`; Starkbot sends its own value) *(verify whether OpenAI expects that originator to be registered)* |
| Scopes | `openid profile email offline_access api.connectors.read api.connectors.invoke`, space separated |
| Callback | `http://localhost:1455/auth/callback` — fixed port, no port fallback. Paste-the-redirect-URL is the fallback when the browser cannot reach this machine |
| Token · refresh | `POST https://auth.openai.com/oauth/token`, **form** body, standard grant params, 15 s timeout; response `access_token`, `refresh_token`, `expires_in` (seconds) |
| Account id | the `chatgpt_account_id` claim inside the `id_token`'s `https://api.openai.com/auth` object; email and plan come from the same profile step. Read for identity only — never trusted for authorization |
| Inference | `https://chatgpt.com/backend-api`, path `/codex/responses` (`/responses` is the non-Codex surface); `Authorization: Bearer <access>`, `ChatGPT-Account-Id`, `originator`, `session_id`, `version`, `OpenAI-Beta: responses=experimental` |
| Allowance | the backend's own usage response behind the same base URL and the same two headers — plan windows and credits, never dollars *(verify the exact usage path)* |

Login is: Settings/Onboarding → **Connect ChatGPT** → `OauthFlow::start(&OPENAI_CODEX)` opens the authorize URL in the default browser and serves the loopback callback on `localhost:1455` (the redirect URI must match `http://localhost:1455/auth/callback` exactly) until the code arrives → exchange → the credential blob goes straight into `neo-keys` account `openai-codex`, and the redacted `{status, email?, plan_type?, workspace?, allowance?, updated_at}` row goes into `provider_accounts`. Disconnect clears the Keychain account and the row. Every rule in the *Limits and accounting* section below applies unchanged to this path.

### Codex app-server fallback

#### Supported integration surface

Neo ships a pinned build of OpenAI's Apache-2.0 Codex binary as a signed nested helper and launches `codex app-server --listen stdio://`. This is the documented custom-client surface, and on **this** path the helper is the only thing that talks HTTP to OpenAI: the runtime speaks JSON-RPC to it and holds no token. (The primary path above does call the Codex backend directly with Starkbot's own OAuth credential — K7 — which is precisely the trade this fallback exists to avoid.) CI records the upstream version, source commit, archive SHA-256, license and generated JSON schema. Updating Codex is an explicit dependency PR that runs the provider conformance suite.
The public repo commits `third_party/codex/manifest.toml` with the upstream version, source commit, release URLs, archive SHA-256 values, and license URL/digest—**not** a platform binary. `neo dev fetch-codex` downloads the pinned official artifact for the host architecture, verifies the declared byte count and SHA-256, extracts only the expected binary, verifies `--version`, and fetches the checksum-verified Apache-2.0 license into the same gitignored vendor directory. Protocol fixture tests are generated from the pinned app-server schema. Release CI repeats verification, signs the nested helper, and bundles it.

The helper gets a dedicated `CODEX_HOME` under Application Support and `cli_auth_credentials_store = "keyring"`. It does not reuse or mutate the user's normal `~/.codex` session. The app initializes with client name `starkbot_neo`; before claiming Enterprise support, register that client identity with OpenAI as its app-server guidance requests.

#### Login and account lifecycle

1. Settings/Onboarding → **Connect ChatGPT** starts app-server and sends `account/login/start {type:"chatgpt"}`.
2. Neo opens the returned `authUrl` in the user's default browser. Codex owns the loopback callback, exchanges the code, stores/refreshes tokens and emits `account/login/completed` plus `account/updated`.
3. Neo stores only `{status, email?, plan_type?, workspace?, allowance?, updated_at}` in SQLite. **On this fallback path** no token crosses the JSON-RPC boundary — that is the whole point of choosing it over the OAuth path (K7).
4. `account/read` checks state; `account/logout` disconnects this app's dedicated session. Device-code login is an accessibility fallback.
5. `account/rateLimits/read` and update notifications drive a plan/allowance card with used percentage and reset time. Reaching the limit pauses new Sol work and offers **wait**, **switch to API key**, or **change model**; switching to a billable key is explicit.

The screen says plainly: content sent through this path follows the user's ChatGPT workspace controls and data settings, not API-platform retention settings. Business/Enterprise workspace restrictions and model availability are authoritative.

#### Tool and process boundary

At `thread/start`, `CodexRuntime` registers the task's snapshotted Starkbot tools as experimental `dynamicTools`. An `item/tool/call` request is schema-validated, dispatched through the same `ToolDispatcher` and `Gated<T>` as metalcraft, then answered with bounded content items. Dynamic tools never gain a second implementation of browser, AX, media, canvas, pack or confirm logic.

Codex is coding-oriented and normally has local tools; those are not an acceptable Starkbot surface. Every thread therefore uses a dedicated empty working directory, `approvalPolicy: "never"`, a `readOnly` sandbox with restricted read roots, and no command-network access. The system instruction permits only the supplied dynamic tools. Any `commandExecution`, `fileChange`, MCP/app call, permission request or unknown effect item causes immediate `turn/interrupt` and `ProviderError::Protocol`; neo never approves it. Only agent messages, reasoning summaries and dynamic-tool calls are accepted.

`dynamicTools` is currently an app-server experimental API. The ChatGPT runtime is therefore badged **Beta** until OpenAI stabilizes it. The pinned-version conformance test must prove login, refresh, model listing, strict output schema, each tool-result content kind, cancellation, allowance reporting and the forbidden-item interrupt. Failure disables only `chatgpt-codex`; an API-key runtime remains available if configured.

### Limits and accounting (both ChatGPT paths)

- ChatGPT allowance is not an OpenAI API dollar balance. Calls record tokens when reported plus `Usd::Unpriced`; the UI shows plan rate windows, not a fictitious dollar cost.
- Dollar caps cannot constrain included-plan usage. Step, decision, wall, tool and external-app action caps still apply; ChatGPT rate-limit exhaustion is a separate hard stop.
- Only models returned/accepted by the connected Codex account are selectable. No mapping claims that every OpenAI API model exists in ChatGPT.
- ChatGPT auth cannot call speech, Jev or user-installed pack endpoints and is never exposed to a media app. Those use independently owned credentials.
- No automatic fallback between subscription and API billing within a task or after a 429. The user starts a new task after explicitly choosing another runtime.

## Connect Claude — subscription OAuth (primary), Claude Code CLI (fallback)

### Subscription OAuth adapter — `MetalcraftRuntime<AnthropicOauthInference>` (K7 · A25)

Starkbot runs this login itself and holds the credential. Every wire fact below is grep-confirmable in the OMP bundle (auth provider id `anthropic`, name "Anthropic (Claude Pro/Max)"); anything that is not is marked *(verify)*.

| Step | Wire fact |
|---|---|
| Client id | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` (OMP stores it base64 as `OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl`) |
| Authorize | `https://claude.ai/oauth/authorize` — PKCE `code_challenge_method=S256`, hex `state`, standard authorize params plus `code=true` |
| Scopes | `org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload`, space separated |
| Callback | `http://localhost:54545/callback` — port fallback allowed if 54545 is taken. Paste-the-final-redirect-URL (or the bare code) is the fallback when the browser cannot reach this machine |
| Token · refresh | `POST https://api.anthropic.com/v1/oauth/token`, **JSON** body; the exchange also sends `state`; the refresh request carries `anthropic-beta: oauth-2025-04-20`. Response `access_token`, `refresh_token`, `expires_in` (seconds), refreshed **5 minutes early** |
| Identity | `account.uuid`, `account.email_address`, `organization.uuid`, `organization.name` off the exchange/refresh response; a bootstrap identity call sends `Authorization: Bearer …` with `anthropic-beta: oauth-2025-04-20`. Identity only — never an authorization decision |
| Inference | `https://api.anthropic.com/v1` Messages — `Authorization: Bearer <access>` and **no** `x-api-key`; `anthropic-version: 2023-06-01`; `anthropic-beta: claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,advanced-tool-use-2025-11-20,effort-2025-11-24,extended-cache-ttl-2025-04-11`; the first system block is Claude Code's own instruction string, as the vendor's client sends it *(verify whether the plan rejects a request without it)* |
| Allowance | `https://api.anthropic.com/api/oauth` + `/usage` → `five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet` windows with reset times; never dollars |

Login is: Settings/Onboarding → **Connect Claude** → `OauthFlow::start(&ANTHROPIC_OAUTH)` opens the authorize URL and serves the loopback callback on `localhost:54545` until the code arrives → exchange → the credential blob goes straight into `neo-keys` account `anthropic-oauth`, and only the redacted `{status, email?, plan, allowance, updated_at}` row into `provider_accounts`. Disconnect clears the Keychain account and the row. The Anthropic terms note below applies with more force on this path than on the CLI one: the login is the user's own Claude account driven by Starkbot itself *(verify the current Agent-SDK/consumer terms before any public release)*. Every rule in the *Limits and accounting* section below applies unchanged.

### Claude Code CLI fallback

#### Supported integration surface

Neo ships a pinned build of Anthropic's official Claude Code CLI as a signed nested helper and runs it in headless streaming mode: `claude --print --output-format stream-json --input-format stream-json --verbose` *(verify the exact flag set against Anthropic's headless/Agent-SDK docs; `--include-partial-messages` adds token-level deltas and `--replay-user-messages` acknowledges queued input)*. This is the documented programmatic surface for a host that is neither Python nor TypeScript, and on **this** path the CLI is the only thing that talks HTTP to Anthropic: the runtime holds no token. (The primary path above calls Anthropic Messages directly with Starkbot's own OAuth credential — K7.) Each stdout line is one JSON event — `system/init` (session id, model, tool list, MCP servers), assistant and user messages, `system/api_retry`, and a final `result` message — and `system/init` carries a `capabilities` array, so the adapter feature-detects instead of comparing version strings *(verify)*.

The public repo commits `third_party/claude-code/manifest.toml` with the upstream version, release URLs, archive SHA-256 values and byte counts, and license URL/digest—**not** a platform binary. `neo dev fetch-claude-code` downloads the pinned official artifact for the host architecture, verifies the declared byte count and SHA-256, extracts only the expected binary, verifies `--version`, and fetches the checksum-verified license into the same gitignored vendor directory. Protocol fixture tests are generated from the pinned version's event stream. Release CI repeats verification, signs the nested helper, and bundles it. Moving the pin is an explicit dependency PR that runs the provider conformance suite.

The helper gets a dedicated `CLAUDE_CONFIG_DIR` under Application Support. Claude Code keeps that directory's credentials in a macOS Keychain entry keyed to the directory, so a session with a different config home reads a different entry: the helper never reads or mutates the user's everyday login, settings, history, hooks or MCP config *(verify the Keychain-entry keying on the pinned version)*. `--bare` is **not** used: bare mode skips discovery but still hands Claude the built-in Bash, file-read and file-edit tools, which P3 forbids. Isolation comes instead from the dedicated config home plus `--restricted`, `--tools ""`, `--strict-mcp-config`, a restricted `--setting-sources` *(verify the accepted form for "load nothing")*, `--disable-slash-commands` and a replaced `--system-prompt`.

Anthropic's Agent SDK terms state that, unless previously approved, third-party developers may not offer Claude consumer-account login or subscription rate limits in their own products. Both Claude paths are therefore the **user's own** Claude login on the user's own machine — driven by the official binary here, and by Starkbot's own PKCE login on the primary path (K7), where the gate is sharper because Starkbot performs the login and holds the token. Shipping either publicly requires clearing that identity with Anthropic first — the same kind of gate as registering the `starkbot_neo` client identity with OpenAI *(verify the current terms before release)*.

#### Login and account lifecycle

1. Settings/Onboarding → **Connect Claude** runs `claude auth login` in the dedicated config home. The plain subscription flow is used; `--console` is never passed, because that variant bills an API account instead of the plan *(verify how `auth login` behaves when driven non-interactively)*.
2. The CLI owns the browser round-trip, the loopback callback, the code exchange and refresh. **On this fallback path** no token crosses into Neo — that is the whole point of choosing it over the OAuth path (K7).
3. `claude auth status` reports account state as JSON and exits 0 when logged in, 1 when not. Neo stores only `{status, plan, allowance, updated_at}` in SQLite.
4. `claude auth logout` disconnects this app's dedicated session and leaves the user's everyday login untouched.
5. A Pro or Max plan meters this work against rolling five-hour and weekly windows shared with the user's other Claude surfaces. Reaching a window pauses new Sol work and offers **wait**, **switch to an API key**, or **change model**; switching to a billable key is explicit and starts a new task.

The screen says plainly: content sent through this path follows the user's Claude subscription controls and data settings, not API-platform retention settings. Team/Enterprise workspace restrictions and model availability are authoritative.

#### Tool and process boundary

Claude Code is a coding harness, and its built-in tool set is exactly the surface Starkbot must not have (P3). Every session therefore runs with:

- **built-in tools off** — `--tools ""` disables the built-in set and `--disallowedTools "*"` removes what remains from the model's context, so Bash, Read, Edit, Write, WebFetch, Glob, Grep, subagent and background-session tools are never offered; `--restricted` is passed as well, since it drops command-running tools and WebFetch and ignores the machine's user and project settings *(verify the interaction of these three flags on the pinned version)*;
- **Starkbot tools as one local MCP server** — a stdio MCP server inside the Neo process, passed with `--mcp-config` and made exclusive with `--strict-mcp-config`. Its tool list is the task's snapshot; every call is schema-validated and dispatched through the same `ToolDispatcher` and `Gated<T>` as metalcraft. MCP tools never gain a second implementation of browser, AX, media, canvas, pack or confirm logic;
- **an empty scratch working directory** per task, with no extra roots (`--add-dir` is never passed);
- **no approval prompts** — `--permission-mode dontAsk` with `--permission-prompts none`, so anything that would ask is denied and the model is told not to retry it *(verify: `--permission-prompts` needs a recent CLI version)*. `--dangerously-skip-permissions` is never passed, in any form;
- **no discovered configuration** — no hooks, skills, plugins, subagents, auto memory or project instructions from the machine.

Any command-execution, file-change, permission-request or unknown-effect item — including a `permission_denied` system message or a non-empty `permission_denials` list on the `result` — interrupts the turn and raises `ProviderError::Protocol` (A22). To end a turn Neo closes stdin and sends SIGINT; SIGTERM would leave the turn unfinished *(verify)*. Only assistant messages, thinking blocks and MCP tool calls are accepted.

The pinned-version conformance test must prove login, session resume, model listing, strict `--json-schema` output, each MCP tool-result content kind, cancellation, allowance reporting and the forbidden-item interrupt. Failure disables only `claude-subscription`; any other configured runtime remains available.

### Limits and accounting (both Claude paths)

- Plan allowance is not a dollar balance. Calls record tokens when reported plus `Usd::Unpriced`; the UI shows plan rate windows — rolling five-hour and weekly — not a fictitious dollar cost. The `total_cost_usd` the CLI reports is a local list-price estimate and is never shown as spend on this path.
- Dollar caps cannot constrain included-plan usage, so `--max-budget-usd` is not the control here. Step, decision, wall, tool and external-app action caps still apply; window exhaustion is a separate hard stop.
- Only models the connected plan accepts are selectable. No mapping claims that every Anthropic API model exists on a subscription.
- Subscription auth cannot call speech, Jev or user-installed pack endpoints and is never exposed to a media app. Speech stays OpenAI-API-key only (K6); those surfaces use independently owned credentials.
- No automatic fallback from plan allowance to a billable key inside a task or after a rate-limit event. The user starts a new task after explicitly choosing another runtime.

## StarkRouter wish-list — what it must provide before starkbot-neo switches to one key

**Inference**
1. OpenAI-compatible **Responses API** with **faithful `reasoning.encrypted_content` replay**: items returned by an upstream are accepted back byte-for-byte on the next turn, and a conversation is **never rerouted to a different upstream mid-conversation** (a rerouted replay is a hard 400 — the luna lesson). Pinning is per conversation; failover only at conversation start.
2. **Reasoning summaries** (`reasoning.summary: "auto"`) passed through, and **streaming** of output-text and reasoning-summary deltas with OpenAI's event names.
3. Function calling, `parallel_tool_calls: false`, image input, image parts in tool results, prompt-cache behaviour and cached-token counts passed through untouched.

**Speech**
4. `/audio/transcriptions` that **honours `prompt` and `keywords`** — forwarded, not dropped — plus **streaming STT** (the Realtime transcription session, WebSocket passthrough).
5. `/audio/speech` with **`pcm` streaming** (24 kHz s16le, chunked, first byte < 300 ms added latency), `voice` and `instructions` passed through.


**Catalog and money**
6. `GET /models` with **capabilities** (use case, modalities, reasoning, context), **live prices**, deprecation flags, and a **`sol-latest` alias** resolved server-side (the response names the concrete model).
7. **Exact `usage.cost` in USD on every inference and speech response**, streamed or not.
8. A **key-info endpoint**: label, usage today / this month, limit, remaining, expiry.
9. **Per-key spend limits set at creation** (daily and total), enforced server-side with a distinct error code—the backstop behind K5.

**Sign-in and privacy**
10. **OAuth PKCE sign-in from a desktop app with a `127.0.0.1` loopback callback** → a minted, revocable, per-device key. Nobody pastes anything.
11. **No prompt logging by default**; metadata-only accounting; any debugging retention is opt-in per key.

Until items 1, 4, 5 and 7 are demonstrably true, the direct API-key runtimes remain available. StarkRouter may replace the OpenAI API-key path; it never impersonates or replaces either subscription runtime.

## How onboarding collapses

| | Today | With StarkRouter |
|---|---|---|
| Onboarding | Welcome → **one inference connection: OpenAI API key · Anthropic API key · Connect ChatGPT (OAuth) · Connect Claude (OAuth) · or either vendor-CLI fallback** → TypeSafe key → optional OpenAI speech key if not already supplied → Microphone/typed-only choice → Accessibility → listening statement | Welcome → **any of the six connections, or sign in with StarkRouter** → TypeSafe key *(gone too if StarkRouter fronts Jev)* → speech/typed-only choice → Microphone → Accessibility → listening statement |
| Spend meter | `Estimated` | `Exact` |
| Model registry | id patterns + fetched `prices.json` | capabilities and prices from `/models`; `sol-latest` server-side |
| Code change | — | one `StarkRouter` `InferenceProvider` wrapped by `MetalcraftRuntime`, one `starkrouter` key account, settings option added. No tool/schema change; both subscription runtimes remain independent. |

Direct vendor keys (OpenAI, Anthropic), the two subscription OAuth connections and the two vendor-CLI fallbacks all stay supported after StarkRouter ships.

## Design input

[research/openrouter-2026-09.md](research/openrouter-2026-09.md) records useful design input—PKCE sign-in with a loopback callback, `usage.cost`, key info, model pricing and aliases—and the gaps that disqualify it here, especially reasoning replay/provider pinning and incomplete speech behavior. Starkbot Neo does not integrate with OpenRouter.
