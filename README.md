# Starkbot Neo

Starkbot Neo is a local-first macOS agent in active development. The target product combines fast browser and macOS accessibility control, SEO workflows, voice, and an agentic Hypercanvas for static and animated creative work.

Work is currently in Milestone 1. The Rust workspace, SQLite store, CLI, terminal front end, desktop Connections shell, subscription bridges, the web and accessibility navigators and the agent loop over them are runnable; the queue, media pipeline and Hypercanvas are planned but not yet released.

## Requirements

- macOS on Apple Silicon or Intel
- Rust stable (the repository toolchain file pins the required version)
- A Claude Pro/Max or eligible ChatGPT account for subscription-backed inference, and a TypeSafe key for the navigator

## Run

Check the machine first: `neo doctor` names every connection, permission and
missing key, and exits non-zero when something has to be fixed.

```sh
cargo run -- doctor
cargo run -- account --provider anthropic-oauth login   # the Claude subscription
cargo run -- ask "Reply with exactly: bridge works"
```

The three front ends over the same runtime:

```sh
cargo run -- tui                      # terminal front end, full agent loop
cargo run -- gui                      # desktop app, Vite dev server on :1420
cargo run -- nav https://example.com "click the More information link"
cargo run -- app com.apple.Numbers "pick the Blank template"
```

`cargo run` is `neo`: the workspace's `default-members` names `crates/neo-cli`,
which is also why a bare `cargo build`, `cargo test` or `cargo clippy` covers
that package alone. Add `--workspace` for the whole thing, as CI does.

`neo gui` runs `cargo tauri dev` in the checkout it finds, because a
development desktop build loads its window from the dev server and starting
the binary alone shows an empty one. `neo gui --build` builds it instead.

`neo nav` and `neo app` drive one surface directly through the navigator —
a web page over CDP, a native app over the accessibility path — with the same
policy, element budget and safety heads the agent loop uses. An app is named by
bundle id, pid, or a substring of its name, and is launched if it is not
running.

### Which runtime the agent loop runs on

`settings.models.inference.provider` picks it, and three of the five provider
ids drive a turn:

| provider | credential | what it is |
| --- | --- | --- |
| `anthropic-oauth` | `neo account --provider anthropic-oauth login` | a Claude Pro/Max subscription on Anthropic's own Messages API — the default |
| `openai-codex` | `neo account --provider openai-codex login` | a ChatGPT Plus/Pro plan on the Responses API |
| `openai` | `neo keys set openai` | an OpenAI API key |

All three reach the same ReAct graph over the same tools, stream the same
token deltas and publish the same events, because all three are `rig`
completion models — the subscription paths differ from the key path only in
which credential the transport puts on the request.

`anthropic` (an Anthropic API key) and `claude-subscription` do not. The
second is the `claude` CLI, which keeps its tools inside its own sandbox and
never hands one back for Starkbot to run, so it cannot drive a browser or an
application; a turn selecting it is refused with that sentence and pointed at
`anthropic-oauth`, which reaches the very same subscription.

The ChatGPT plan needs one setup step of its own: `neo dev fetch-codex`
downloads the official Codex CLI release pinned in
`third_party/codex/manifest.toml`, verifies its SHA-256 checksum and records
its Apache-2.0 license beside the binary under ignored `vendor/`.
Authentication stays in the vendor's own storage; Starkbot Neo persists only
redacted account status. API keys are a separate provider path
(`neo keys set …`) and are not required for subscription use.

## Tracing

Starkbot Neo is OpenTelemetry-instrumented. Set `OTEL_EXPORTER_OTLP_ENDPOINT`
and it exports spans over OTLP/HTTP with JSON encoding to whatever is
listening there: an OpenTelemetry Collector, Jaeger, Tempo, Honeycomb,
Raindrop, or [Starkbot Trace](https://github.com/ethereumdegen/starkbot-trace),
which is a separate program with no build-time relationship to this
repository — it receives from Neo the way it would from any instrumented
program. With no endpoint set there is no exporter and no background task, and
every `neo-otel` entry point returns after a single atomic load.

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
  cargo run -- ask "Reply with exactly: otlp works"
```

| variable | meaning |
| --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | where to post; `/v1/traces` is appended if it is not already there |
| `OTEL_EXPORTER_OTLP_HEADERS` | `k=v,k2=v2`, for a hosted endpoint's key |
| `OTEL_SERVICE_NAME` | overrides `starkbot-neo` in the resource |

The spans, following the OpenTelemetry GenAI semantic conventions where they
exist:

| span | one per | attributes |
| --- | --- | --- |
| `invoke_agent` | agent turn | `gen_ai.operation.name`, `starkbot.turn`, `starkbot.user_text`, `starkbot.history_len`, `starkbot.max_steps`, `starkbot.steps`, `starkbot.exhausted`, `starkbot.asked`, `starkbot.answer` |
| `execute_tool` | chosen action | `gen_ai.tool.name` (`browse`/`app`/`answer`/`ask`), `gen_ai.tool.call.id`, `starkbot.target`, `starkbot.goal`, `starkbot.thought`, `starkbot.observation` |
| `chat {model}` | inference call | `gen_ai.operation.name`, `gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `starkbot.json_mode`, `starkbot.prompt_chars` |
| `navigate {browser\|app}` | navigator run | `starkbot.surface`, `starkbot.target`, `starkbot.goal`, `starkbot.outcome`, `starkbot.steps` |
| `jev {OPERATION}` | navigator step | `starkbot.operation`, `starkbot.confidence`, `starkbot.candidates`, `starkbot.stale`, `starkbot.label`, `starkbot.typed_chars`, `starkbot.observe_ms`, `starkbot.jev_ms`, `starkbot.text_ms`, `starkbot.act_ms` |

A turn parents the steps, model calls and navigator runs it caused; a
navigator run parents its decisions. Work started outside a turn — `neo ask`,
a bare `neo nav` — is its own root. Every `AppEvent` the core publishes
becomes a span event on whatever span is open. A failed turn, inference or
run carries the error as the span's status.

Prompts are counted, never copied; goals, thoughts, observations and answers
are recorded, because they are what a trace is for. No credential ever
reaches an attribute, and text typed into a field is counted
(`starkbot.typed_chars`), never kept.

## Project plans

- [`PLAN.md`](PLAN.md) — milestone sequence and current status
- [`plans/`](plans/) — architecture and product contracts
- [`plans/08-providers.md`](plans/08-providers.md) — account, provider, and cost policy
- [`plans/11-hypercanvas.md`](plans/11-hypercanvas.md) — Hypercanvas architecture

## Status and safety

This is pre-release software. Review commands before using it against important repositories or accounts. Generated changes remain subject to the permissions and confirmation rules documented in the plans.

## License

Starkbot Neo is MIT-licensed. Downloaded Codex artifacts are Apache-2.0 and retain their own license.
