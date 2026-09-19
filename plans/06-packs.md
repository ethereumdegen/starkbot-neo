# 06 — Packs: skills, HTTP tools, personas, routines (`neo-packs`)

Everything starkbot-neo knows beyond its core loop — the navigator, Sol, the gates — is a **pack**. Decision A11.

## 1. Principles

1. **Portable data packs are data, never code.** JSON + Markdown. Nothing in the base pack surface executes; no scripts, no selectors, no shell (P3). A pack can *describe* an HTTP request, *advise* Sol in a skill, *name* a recipe of neo's own gated tools — nothing else. M10's explicit `component/` tier is sandboxed Wasm, separately badged and consented (§12), never treated as a data pack.
2. **Same format as the user's other hosts.** The Axoniac agent-pack format, `manifest_version: 2`, as specified in `metalcraft-agent/specs/AGENT_PACK_FORMAT.md` and consumed by metalcraft-agent, starfire and degen-tools. A data pack published for those hosts installs in neo **unchanged**; a data pack authored for neo stays valid for them.
3. **Neo-only extensions are namespaced paths.** `desktop/` is declarative native-app hints, routines and tighten-only policies (§4). Optional `component/` is the M10 sandboxed-code tier (§12). Other hosts carry, hash and ignore both.
4. **Consent is computed from the bytes, never declared** (spec §5); neo re-derives everything it shows. **Packs advise; they never authorize.** No pack part can remove a deny rule, lower a threshold, skip a gate or see another pack's credential.
5. **No website knowledge, ever (P9).** No pack part may carry per-site selectors, landmarks, URLs-to-click or layouts. Skills describe *workflows*; routines carry *goal strings*; the navigator works out the page.

## 2. The shared format, exactly as found in the real packs

Read from `axoniac-seeded-agent-packs/packs/{vercel-vgpu, taste-skill, octaweave, cloudflare-domains, buildr-space}`, `packctl` (`axoniac-seeded-agent-packs/src/lib.rs`), `metalcraft-agent/src/agent_packs/{bundle,manifest}.rs`, `metalcraft-packs`, and `degen-tools/src/{tool,install,run}.rs`.

```
<id>-<version>.agentpack                (zip, DEFLATE; ≤ 64 MiB decompressed, measured on bytes read)
  agent_pack.json                       manifest — written last, excluded from its own hash
  agent_presets/<slug>.json             exactly one
  agent_presets/<slug>/memories.jsonl   optional seed memories      (neo: ignored in v1)
  personas/<slug>.json                  every persona the preset names
  skills/<slug>.md                      every skill the preset or its personas load
  integrations/<id>/integration.json    every integration the preset or its personas reach
  integrations/<id>/api_tools/*.json    one HTTP tool per file; file stem == tool name
  integrations/<id>/README.md           optional
  flows/<id>.json                       optional                    (neo: ignored in v1)
  SIGNATURE                             optional, ed25519 over agent_pack.json (ship now, enforce later)
  desktop/…                             neo-only (§4)
  component/manifest.json              optional neo component capability manifest (M10)
  component/component.wasm             optional `wasm32-wasip2` component (M10)
```

A pack is **self-contained**: installing needs no network. A pack may have zero integrations (`taste-skill`) or zero skills, but the shared validator demands **exactly one preset whose default persona is in the archive** — so even a desktop-only neo pack ships a minimal preset + persona (`neo pack new` scaffolds them).

### 2.1 `agent_pack.json`

| Field | Req | Meaning |
|---|---|---|
| `manifest_version` | yes | must be `2`; anything else is rejected |
| `id` | yes | `^[a-z0-9][a-z0-9_-]{0,63}$` |
| `handle` | no | registry handle (`vercel_vgpu`); registry paths use it, falling back to `id` |
| `name`, `version` | yes | display name; semver |
| `tagline`, `description`, `license`, `category`, `tags[]`, `author{handle,display_name,sub}` | no | catalog text |
| `presets[]` | yes | exactly one slug |
| `provides.personas[]`, `provides.skills[]` | no | slugs |
| `provides.integrations[]` | no | `{id, version, content_sha256?, source?}` — the hash pins the vendored integration's bytes |
| `requires_env[]` | derived | `{name, needed_by[], required}` — written by the builder, **re-derived at install** |
| `domains[]` | derived | hosts of every tool URL |
| `content_sha256` | derived | `metalcraft_packs::canonical_sha256` over every file except the manifest |
| `parent` | no | fork lineage `{id, version, content_sha256}` |
| `replaces` | no | neo-only id of a replaceable data pack; allowed only on a local clone whose `parent` names that pack |

Unknown top-level fields are preserved and never cause rejection.

### 2.2 `personas/<slug>.json`

| Field | Meaning |
|---|---|
| `name`, `description`, `version` | display; optional semver |
| `tools[]` | host tool names the persona expects (`load_skill`, `ask_user`, `bash`, `read_file`, `write_file`, `edit_file`, `list_files`, `find_files`, `grep`, `web_fetch`, `update_plan`, …) |
| `integrations[]` (alias `packs`, `integration_packs`) | integration ids whose tools it may call — both spellings occur in the real packs |
| `skills[]` | skills offered to `load_skill` |
| `system_prompt` | the persona's prompt text |
| `max_run_secs` | sub-agent run bound on other hosts (clamped 1800) — neo ignores it; neo's own task caps apply |

### 2.3 `agent_presets/<slug>.json`

| Field | Meaning |
|---|---|
| `manifest_version` | `2` (absent / `1` still accepted on read) |
| `slug`, `name`, `tagline`, `description`, `avatar` | display |
| `default_persona` | required; must exist in the archive |
| `personas[]` | `{slug, role: default \| subagent \| internal, description?}`; at most one `default` |
| `skills[]`, `integrations[]`, `flows[]` | what the preset declares |
| `model` | `{tier: premium \| standard \| fast, needs[], min_context, prefer?}` — a **capability floor, not a model name**; an unmet floor warns, never refuses |
| `memories`, `requires_env`, `version` | optional |

**`skills/<slug>.md`** — YAML frontmatter then Markdown. Frontmatter keys found: **`description`** (required in practice — it is the index line) and **`version`** (optional semver). The slug is the filename. The body is the knowledge, vendored as copies (no links out).

### 2.4 `integrations/<id>/integration.json`

| Field | Meaning |
|---|---|
| `id`, `name`, `description`, `version` | `id` must equal the directory name |
| `requires_env[]` | **the only credential names this integration's tools may use** |
| `tags[]`, `icon` | classification |
| `native_tools[]` | names of tools implemented natively in the host binary rather than as `api_tools` files (exists today in `metalcraft_packs::IntegrationManifest`; §8 relies on it) |
| `key_help` | **neo extension** (§7.3); unknown to other hosts and ignored by them |

Note: `allowed_hosts` is a **per-tool** field, not an integration field.

### 2.5 `integrations/<id>/api_tools/<tool>.json`

| Field | Default | Meaning |
|---|---|---|
| `name`, `description` | — | name must equal the file stem; the description is what Sol reads and what the gate sentence is built from |
| `method`, `url` | — | URL may hold `{param}` placeholders and may start with `$BASE_URL_ENV` for self-hosted services |
| `headers{}` | `{}` | values may reference `$NAME` / `${NAME}` |
| `parameters` | empty object schema | JSON Schema; **every property must carry `type`, `enum`, `$ref` or a composition keyword** — one untyped property makes a provider refuse the whole schema (packctl enforces this) |
| `body_mapping` | `params` | `none` · `params` (args minus URL params, merged over `body_defaults`) · `params_nested` (uses `param_paths{arg → dotted.path}`) · `template` (uses `body_template`) · `multipart` (agent only — neo marks such a tool *unsupported*) |
| `body_defaults{}` | `{}` | fixed body fields; string values may hold declared `$NAME`s |
| `timeout_secs`, `poll` | 30, false | timeout clamped to 600; `poll` marks a status-check tool exempt from tight-loop detection |
| `error_path` | — | GraphQL-style: a non-empty value at this path fails a 200 response |
| `allowed_hosts[]` | — | hosts the tool may reach (`*.neon.tech` = any subdomain); **required** when the host comes from a parameter and a credential is sent |
| `secret_paths[]` | — | response paths masked in every output |
| `save[]`, `save_inline[{path, ext}]` | — | response paths holding media URLs to download / inline content to write to a file |
| `mutating` | — | **neo extension** (§6): `false` opts a safe non-GET out of gating |

## 3. How each part maps into neo

| Pack part | In neo |
|---|---|
| `skills/*.md` | One fixed tool `load_skill(name)`. Sol's system prompt carries only an **index** of enabled skills (`name — description`, truncated to 160 chars), sorted, snapshotted at task start. The body enters context only when loaded. Skills are never shown to Jev or the text helper. |
| `integrations/*/api_tools` | HTTP tools run by `neo-packs::http`, a port of the degen-tools runner (§5), reached only through the three meta-tools (§5.1). |
| `requires_env` | One Keychain item per declared name, entered through the enablement flow (§7). |
| `personas` | A **mode**. Prompt order: neo base rules (trust boundary, gates) → `soul.md` (P8) → the persona's `system_prompt` inside a delimited *mode* block stated to rank below the base rules → skill index. A mode narrows the skill index and pack-tool scope to the persona's lists via `allowed_tools`; it never changes the tool list. Picked per task in the composer, by voice ("Stark, designer mode"), or left at *none*. |
| persona `tools[]` | Mapped by name. neo has `load_skill`, `ask_user`. It **does not have and will not get** `bash`, `read_file`, `write_file`, `edit_file`, `list_files`, `find_files`, `grep`, `web_fetch`, `update_plan`, `sub_agent` (P3). Missing tools are **dropped**, the pack is badged ***partially supported*** with the dropped names listed, and the mode block gains one line: "these tools do not exist here: …; do not attempt them." Skills and HTTP tools of such a pack still work. (`vercel-vgpu`, `taste-skill`, `buildr-space` all land here.) |
| preset `model` | `tier` → model picker default for that mode: `premium → sol`, `standard → terra`, `fast → luna` (latest of each family from the model registry). `needs` / `min_context` are checked against the registry; unmet → install warning. A user-pinned model always wins; `prefer` is never a requirement. |
| `flows/`, `memories.jsonl`, `vectors.bin`, persona roles / sub-agent roster | Carried, hashed, **ignored** in v1. Listed under "not used by starkbot-neo" in pack detail. |
| `desktop/apps`, `desktop/routines`, `desktop/policies.json` | §4. |
| `component/manifest.json`, `component/component.wasm` | M10 sandboxed component extension (§12); absent for ordinary data packs; carried, hashed and ignored by other hosts. |

## 4. The `desktop/` extension (neo only)

```
desktop/
  apps/<bundle-id>.json     optional NATIVE-app technical hints
  routines/<name>.json      declarative recipes matched by Jev at intake
  policies.json             extra deny / must-confirm / pacing rules — tighten only
```

### 4.1 `desktop/apps/<bundle-id>.json` — native-app hints

Technical facts the accessibility tree cannot express, for **native apps only**. Never for websites: no origins, no URLs, no selectors, no landmarks, no element paths — the schema has no such field and `additionalProperties: false` rejects attempts (keys matching `landmark|selector|xpath|css|url|origin` fail validation by name).

```json
{ "bundle_id": "com.tinyspeck.slackmacgap", "name": "Slack",
  "ax_strategy": "manual_accessibility", "settle_ms": 350,
  "shortcuts": { "quick switcher": "cmd+k", "new message": "cmd+n" },
  "confirm_labels": ["Send now", "Delete message"],
  "snapshot": { "drop_roles": ["AXSplitter"], "collapse_roles": ["AXGroup"], "max_rows": 40, "max_depth": 18 },
  "skills": ["slack-basics"] }
```

| Field | Use |
|---|---|
| `ax_strategy` | `none` · `enhanced_ui` (`AXEnhancedUserInterface`) · `manual_accessibility` (`AXManualAccessibility`, Electron) — which flag `neo-ax` sets before observing |
| `settle_ms`, `snapshot` | post-action settle time for `AxObserver` (20–2000 ms); pruning hints for the AX snapshot |
| `shortcuts` | offered to the navigator as named `PRESS_KEY` options for that app, and listed to Sol in manual fallback |
| `confirm_labels` | added to the must-confirm label list **for that app only** |
| `skills` | skills starred in the index when this app is frontmost |

**Hints are an optimisation, never a dependency.** Every native scenario runs in CI-recorded and pre-release live form twice — normally and with `--no-hints` (all `desktop/apps` ignored, built-in defaults: `ax_strategy` probed, `settle_ms` 300). **Both must pass**; hints may only make a run faster. Browser bundle ids may appear here for per-*browser* facts only (AX flag for Safari/Firefox, tab shortcuts).

### 4.2 `desktop/routines/<name>.json` — routines

A routine is a parameterised recipe of neo's own gated tools. **Jev picks it, the text helper fills its parameters, Rust runs it, Jev verifies it, Sol is the fallback.** Zero Sol calls on the happy path.

```json
{
  "name": "log_crm_activity",
  "description": "Log a call, meeting or note against a contact in the CRM that is open in Stark's Chrome",
  "examples": ["log that call with Dana, next step demo Friday", "note on Acme: they want annual billing"],
  "params": { "type": "object", "required": ["contact", "summary"], "properties": {
      "contact": { "type": "string", "ask": "Who is this about?" },
      "kind":    { "type": "string", "enum": ["call", "meeting", "note"] },
      "summary": { "type": "string" },
      "next_step": { "type": ["string", "null"] } } },
  "steps": [
    { "tool": "navigate", "args": { "goal": "In the CRM open in this tab, find the contact {contact}, open their record, and add a {kind} activity with the text: {summary}. Next step: {next_step}. Save it. Done when the new activity is visible on the record." } }
  ],
  "verify": "The contact's record shows a new activity containing the given summary.",
  "on_fail": "handoff",
  "max_secs": 90
}
```

A native one is the same shape: `focus_app {name:"Notes"}` → `key {combo:"cmd+n"}` → `type_text {text:"{title}\n{body}"}`. A routine that targets the web does so **only** through a `navigate` step whose `goal` is a sentence (plus optional `start_url` the *user* supplied as a parameter or a pack-declared product home page) — still no selectors, no per-site steps.

| Field | Rule |
|---|---|
| `name`, `description`, `examples[]` (≤ 8) | slug unique within the pack, addressed as `<pack>/<name>`; description + examples are what the Jev head reads |
| `params` | JSON Schema object; every property typed; optional `ask` = the question used when a required value is missing |
| `steps[]` (1–12) | `{tool, args}`; string args may hold `{param}`; **no loops, no conditionals, no refs** |
| step `tool` | closed list: `navigate`, `open_url`, `focus_app`, `launch_app`, `key`, `type_text`, `select_menu`, `wait_for`, `extract`, `media_import`, `media_export`, `canvas_export`, `call_pack_tool` (tools of the routine's **own** pack only; `neo-user` may name any enabled pack's). Paid `media_*` generation and `canvas_apply` are not allowed — generation is Sol's job |
| `verify`, `on_fail`, `max_secs` | one sentence, required · `handoff` (default) or `fail` · ≤ 300 |

**Dispatch.** The intake TypeSafe request (A7) carries a **`routine` head** beside `intent` and `route`: a `choice` over enabled routines (`<pack>/<name> — description · e.g. example`) plus `none`. More than 40 enabled routines → a local lexical pre-rank keeps the top 40. Route becomes `routine:<name>` when `intent = new_task` ∧ `P(routine) ≥ 0.75`; otherwise the normal route stands. No extra round trip.

**Parameters.** One text-helper call (`gpt-5.6-luna`, reasoning off, strict `json_schema` = the routine's `params`) over `{utterance, last 3 messages, selection facts}`; rule "use `null` when the user did not say it; never invent". Skipped when `params` is empty. A missing required value → the param's `ask` sentence as a question in the thread (`waiting_user`); the answer re-runs extraction. Result is validated against the schema in Rust.

**Execution.** Each step runs through the **same gated tools** as any task: deterministic rules → safety heads → confirm card. A `navigate` step is a full navigator run with its per-step heads. A routine gets no gate exemptions.

**Verify and failure.** After the last step, one Jev `yes_no` on the `verify` sentence over the final observation (navigator's last page state / AX diff / pack-tool result). `≥ 0.6` → `done`. Jev unreachable → `unknown`, reported as such, not as success. Step error, `BLOCKED`, verify `< 0.6` or `max_secs` hit → `on_fail`. `handoff` re-routes the task to Sol with the routine JSON, each step's result, which mutating steps **already happened** (never blindly repeated), and the last observation.

"Save what I just did as a routine" (Sol distils a trace into a routine the user reviews; lands in `neo-user`) is post-M9.

### 4.3 `desktop/policies.json` — tighten-only

```json
{ "deny_apps": ["com.example.bank"], "deny_origins": ["*.examplebank.com"],
  "confirm_apps": ["com.apple.mail"], "confirm_labels": ["Boost post", "Promote"],
  "thresholds": { "outward": 0.3, "spends": 0.25 },
  "pacing": { "*": { "max_actions_per_hour": 120, "min_delay_ms": 900 } },
  "caps": { "task_usd": 0.5 } }
```

Merge with the rules layer (A9), per key: **lists → union**; confirm thresholds (`outward`, `destructive`, `spends`) → **min**; `on_task` floor → **max**; pacing `max_*` → **min**, `min_delay_ms` → **max**; spend caps → **min**. There is no `allow_*`, no `remove_*`, no way to name an existing rule. A value that would loosen the current *defaults* (e.g. `outward: 0.6`) is a **validation error at install**, not a silent no-op. Every merged rule keeps its provenance (`from pack neo-gtm`) in Settings → Safety; disabling the pack removes exactly its contributions. Origin patterns here are *policy*, not navigation knowledge — they say where the bot must stop or slow down, never how to operate a site.

### 4.4 Do the other hosts tolerate neo-only paths? — verified in source

| Host | Finding |
|---|---|
| metalcraft-agent `Bundle::read` | reads **every** zip entry into a `files` map, checks only path safety + size, hashes all of it, then interprets known prefixes (`agent_presets/`, `personas/`, `skills/`, `integrations/`, `flows/`). No path allow-list; no `deny_unknown_fields` on manifest, persona, preset or integration structs. **Tolerates `desktop/`; it is covered by `content_sha256`.** |
| `packctl` (`collect_files`) | zips and hashes every non-dotfile under the pack dir → `desktop/` is built into the `.agentpack` with no change. |
| axoniac registry (`axoniac-prime/backend/src/services/bundle.rs`) | prefix-driven like the agent; unknown paths contribute nothing and are not rejected. |
| degen-tools `stage_one` | keeps only `integration.json`, `README.md`, `api_tools/*.json`, `skills/*.md`; everything else never reaches disk. Tolerates. (It refuses a pack with zero integrations — correct: nothing for it to run.) |
| The spec text | §2 lists the layout but states no rule for unknown paths (§3 does, for unknown manifest *fields*). **Needed:** one sentence in spec §2 — "unknown paths are carried, hashed and ignored; `desktop/` and `component/` are reserved for starkbot-neo" — constitution open item 5. |
| Consequence | other hosts' consent summaries do not mention `desktop/` or `component/`. They cannot execute either; neo derives its own consent (§9.3, §12). The unknown tool field `mutating` and integration field `key_help` are likewise ignored elsewhere. |

## 5. HTTP tools: the ported runner

`neo-packs::http` is a port of `degen-tools/src/tool.rs` + `run.rs` (the most hardened copy of metalcraft-agent's `http_api.rs`), made async and Keychain-backed:

- URL `{param}` expansion: path values raw (a model id keeps its slashes), query values percent-encoded, **empty/null optional query params dropped** with their key.
- Body: `none` / `params` / `params_nested` / `template`; args consumed by the URL are never also sent in the body. `multipart` → tool listed as unsupported.
- `$NAME` expansion in URL, headers and `body_defaults` **only for names in that integration's `requires_env`**; undeclared `$NAME` stays literal. Arguments are never expanded.
- `error_path` failure detection; `timeout_secs` clamp; `poll` honoured.
- Host check on the URL **as the HTTP client parses it** (defeats `x.neon.tech@evil.example`): must match `allowed_hosts` when present; a tool whose host comes from a parameter and that sends a credential without `allowed_hosts` is refused. neo adds: `https` only (except a host taken from a user-stored `$BASE_URL` credential), **redirects never followed**, loopback / link-local / private addresses refused unless the host is that user-stored base URL, response cap 16 MiB.
- `secret_paths` and every expanded credential value are masked (`••••`) in tool results, the trace, the Mind pane and logs; `display_url` (pre-expansion) is the only URL ever shown.
- `save` / `save_inline`: files go to the task's attachment folder in app support (never an arbitrary path — P3), ≤ 64 MiB each, extension from content-type / a validated ≤ 5-char alnum `ext`; with `neo-media` enabled they are also imported as **takes**. The response shows a short note, not the content.

### 5.1 Three fixed meta-tools

Sol's tool list must never change mid-task (prompt cache) and should not differ between tasks either — and one host already has 268 pack tools. So pack HTTP tools are **not** registered individually:

| Tool | Args | Returns |
|---|---|---|
| `list_pack_tools` | `{ pack?: string, query?: string, limit?: integer ≤ 50 }` | `[{ name, pack, summary (first sentence), method, mutating, ready (keys present) }]` |
| `describe_pack_tool` | `{ name: string }` | `{ name, pack, description, parameters (JSON Schema), method, mutating, hosts[] }` |
| `call_pack_tool` | `{ name: string, args: object, why: string }` | `{ status, data (masked, truncated to 24k chars with a note), saved[]? }` or a gate result (`denied` / `needs_user`) |

`args` is validated against the tool's schema **in Rust** before anything else; a validation error is returned verbatim so Sol can repair the call. Native tools (navigate, extract, media, canvas) stay first-class; which of those exist is fixed at task start by the enabled embedded packs. The **enabled-pack set, skill index, routine list and mode are snapshotted when a task starts** (`TaskSnapshot`); enabling a pack affects only tasks started afterwards.

## 6. Gating pack tool calls

`call_pack_tool` sits behind `Gated<T>` (A9):

1. **Deterministic:** pack disabled / key missing → `needs_user` (offers the enablement flow). `DELETE` → must-confirm always. Tool name or first description sentence matching the global confirm labels → must-confirm.
2. **`GET` → ungated** (read-only by construction; still host-checked and masked).
3. **Everything else → one TypeSafe request** with heads `on_task`, `outward`, `destructive`, `spends`. State = action sentence built from the tool description + masked args + `why`; facts = task, amendments. `on_task < 0.3` → denied; any risk head `≥ 0.4` → confirm card showing pack, tool, host, masked args. Jev unreachable → **fail closed** → confirm card.
4. **`"mutating": false`** on a tool JSON opts a safe non-GET (GraphQL query, search-by-POST) out of step 3. It is an author's claim, so it is **listed by name on the consent screen** ("non-GET tools declared read-only: …") and the deterministic layer still applies.

## 7. Credentials and the enablement flow

### 7.1 Storage and isolation (`neo-keys`)

- One Keychain item per **declared env name** (service `com.starkbot.neo.pack-keys`, account = the name). Secret strings exist only inside `neo-keys` and the request builder; never in the webview, the DB, Sol's context, Jev state or the text helper.
- A pack reads a name only if (a) its integration declares it **and** (b) the user granted that `(pack, name)` pair on its consent screen. A name already stored for another pack prompts: *"This pack will use your existing `FAL_KEY` and may send it to: …hosts…"*.
- Reserved names — `OPENAI_API_KEY`, `TYPESAFE_API_KEY`, anything `NEO_*` — cannot be declared by a non-embedded pack (install refused).
- Exfiltration guard = the three runner rules together: declared-names-only expansion, host check on the parsed URL, no redirects.

### 7.2 The flow — generic, for any pack with `requires_env`

Enable → **what it does** (description + consent summary, §9.3) → one `KeyField` screen **per declared name** (paste-only field in the app UI; optional live validation; → Keychain) → pack-specific limits if its policies define caps → enabled. Partial enablement is allowed per integration: tools whose keys are absent show `ready: false`.

**Keys are never entered by voice**, never typed by the bot, never read back. When a task needs a disabled pack, Sol may *offer* the flow through `ask_user`; a yes opens the UI flow, the task waits in `waiting_user`, and is then **re-queued and restarted** (the tool set is fixed per task). 07's `neo-media` is the worked example.

### 7.3 `key_help` (optional, on `integration.json`)

```json
"key_help": { "FAL_KEY": {
  "label": "fal.ai API key", "url": "https://fal.ai/dashboard/keys", "aliases": ["FAL_API_KEY"],
  "validate": { "method": "GET", "url": "https://api.fal.ai/v1/models/usage",
                "headers": { "Authorization": "Key $FAL_KEY" }, "ok_status": [200, 403], "bad_status": [401] } } }
```

`url` = the "Get a key" link (opened in the user's default browser, not Stark's Chrome). `validate` runs through the same runner rules (host must be in the pack's derived `domains`). No `key_help` → the key is stored unvalidated and flagged "unverified" until its first successful call.

## 8. Embedded first-party packs

Pattern: **JSON pack + native Rust tools.** A capability needing a new protocol is one native tool in Rust plus a pack whose `integration.json` names it in `native_tools`; skills, personas, routines, policies and `requires_env` stay data. Embedded with `include_dir!`, versioned with the app, same validator as third-party packs, badged *built in*.

| Pack | Default | Contents |
|---|---|---|
| `neo-desktop` | **always on** | native tools `navigate`, `extract`, `ask_user`, `load_skill`, the meta-tools, fine-grained AX fallback tools; base policies (deny list incl. terminal-class apps, global confirm labels); skill "working through the navigator" |
| `neo-browser` | on | **generic web skills** (tabs, forms, search → results → detail, pagination, reading a page, consent banners → most privacy-preserving option) + `desktop/apps` hints **per browser** (Safari / Firefox AX flag, tab shortcuts). **Nothing per website.** |
| `neo-apple-apps` | on | hints + routines + skills for Finder, Notes, Mail, Calendar, Reminders, Messages, Music, Keynote; the open/save-panel routine used by media export |
| `neo-electron` | on | hints only: Slack, Discord, Notion, Spotify, VS Code (editor panes; its terminal stays denied) |
| `neo-media` | **off** | native `media_*` tools (07), `requires_env: FAL_KEY, QUIVERAI_API_KEY` + `key_help`, per-model prompting skills, spend policies |
| `neo-gtm` | **on** | workflow skills + routines + pacing policies (09); no native code, no site knowledge |
| `neo-user` | on, **writable** | the user's own routines, native-app hints, vocabulary; edited in the Packs UI; never published implicitly |

## 9. Install, versions, consent, trust

### 9.1 Sources and registry protocol

Embedded · local directory · `.agentpack` file · **registry**. `~/Library/Application Support/com.starkbot.neo/registries.json` follows spec §11.2 (`default`, `registries{name → {url, trust: first-party | verified-only | explicit, token_key?}}`); shipped default `axoniac → https://axoniac.com`, `verified-only` (constitution open item 4).

Protocol (spec §11.1), all `GET` under `/api/v1/agent-packs/`: `{id}/version` (update check → `{id, handle?, version, content_sha256}`) · `{id}/manifest` (preview before download) · `{id}/download` (the zip) · `search?q=&limit=` (browse; optional on the host). `{id}` = handle, falling back to id. References: `@handle`, `registry:@handle`, or an `https://` URL whose origin is a configured registry. **Redirects are never followed; userinfo is stripped before origin compare; an id found in two registries is an error unless qualified; 64 MiB cap on bytes actually read; 60 s timeout.** The optional `POST …/installed` ping is **off** unless the user opts in.

### 9.2 Install order, `neo.lock`, updates

Order: (1) fetch → unzip **in memory** with path-safety (no `..`, absolute, drive letters, symlinks) and size guards → (2) verify `content_sha256` and every `provides.integrations[].content_sha256` **before interpreting or writing anything** → (3) validate (§9.4), reporting **all** problems at once → (4) derive consent (§9.3), user approves → (5) stage to a temp dir, atomic rename to `…/com.starkbot.neo/packs/<id>/<version>/` → (6) write `neo.lock`, insert the `packs` row (`enabled = 0`), run the enablement flow if `requires_env` is non-empty, else enable.

`neo.lock` (JSON beside the DB, npm-style like `metalcraft.lock`): per pack `{version, content_sha256, source{kind, registry?, url?|path?}, integrations[{id, version, content_sha256}], installed_at}`. On every app start each installed pack is re-hashed against the lock; a mismatch **disables the pack** and says so.

Versions are semver. **No auto-update.** `/version` is polled on launch and daily; an available update shows a **consent diff** (new domains, new env names, new / newly-mutating tools, routines whose steps changed, new policies, newly dropped persona tools) and needs an explicit approve. The previous version directory is kept for one-click rollback. `SIGNATURE` is carried and shown as present/absent; verification against `/.well-known/agent-pack-signing.json` is enforced when the spec's v2 turns it on.

### 9.3 Consent screen

The four shared fields, derived exactly as spec §5.1 — **`domains`**, **`requires_env`** (`name`, `needed_by`), **`tools`**, **`mutating_tools`** (method ≠ GET) — plus neo's own: non-GET tools declared `mutating: false`; tools unsupported here (`multipart`); persona tools dropped (→ *partially supported*); native apps hinted; **routines, each with its steps readable**; policies added; model floor met or not; signature present or not; source + hash. A manifest whose stated `requires_env` / `domains` disagree with the derived ones is **rejected**, not warned.

### 9.4 Validation (install and `neo pack validate`)

Spec §10 V1–V17 in full, plus:

- every `api_tools` property typed (packctl's rule); tool name == file stem; `integration.json` id == directory;
- every `$NAME` in a URL / header / `body_defaults` is declared in that integration's `requires_env` (**error**, where degen-tools only warns); no reserved names;
- a credentialed tool has a literal host, `allowed_hosts`, or a declared `$BASE_URL` prefix; `key_help.validate` hosts ⊆ derived domains; `save_inline.ext` plain;
- `desktop/apps`: schema with `additionalProperties: false`; forbidden key names; `bundle_id` well-formed;
- `desktop/routines`: schema; step tools ∈ the closed list; `call_pack_tool` names resolve inside the pack; every `{param}` is declared; no string arg contains a CSS / XPath-looking selector or `javascript:`; `verify` present;
- `desktop/policies.json`: tighten-only check against current defaults.

### 9.5 Trust model

Skill and persona text is **advice to Sol, not authority**: it is never seen by the navigator, Jev or the text helper; it cannot add tools, loosen policy or waive a gate; and every action it leads to still passes `on_task` and the risk heads against the **user's own task**. A malicious skill is contained the same way a malicious web page is (A9: text is data). Pack tool *responses* are untrusted data too. What a third-party pack *can* do: reach its declared domains with its granted keys, propose routines the user can read, make neo stricter. Nothing else.

## 10. `neo-packs` crate

Depends on `neo-core`, `neo-keys`, `neo-store`, `jev-nav::wire` (routine head, verify); `neo-agent` and `neo-judge` depend on it. Tauri-free.

```
crates/neo-packs/src/
  bundle.rs       Bundle::read / from_dir — port of metalcraft_agent::agent_packs::bundle (guards, hash-before-interpret)
  manifest.rs     AgentPackManifest, Preset, Persona, Integration, ToolConfig, SkillMeta (serde; unknown fields kept)
  consent.rs      derive_consent() → ConsentSummary + NeoConsent; consent_diff()
  validate.rs     V1–V17 + neo rules → Vec<Problem> (all at once)
  desktop.rs      AppHints, Routine, Policies (+ JSON Schemas) and tighten-only merge
  registry.rs     PackRegistry: enabled set, TaskSnapshot, embedded packs (include_dir!), skill load + index
  http.rs         runner: prepare → host check → send → error_path → mask → save
  meta_tools.rs   list / describe / call as metalcraft Tools
  routines.rs     routine head options, param extraction, step executor, verify, handoff bundle
  modes.rs        persona → Mode (tool mapping, dropped list, prompt block), model-tier mapping
  install.rs      sources, registry client (no redirects, caps), staging, atomic swap, rollback, neo.lock
  enable.rs       enablement state machine, key_help validation, grants
```

```rust
pub struct PackId(String);   // validated slug
pub enum PackSource { Embedded, Dir(PathBuf), Archive(PathBuf), Registry { name: String, url: Url } }
pub enum Support { Full, Partial { dropped_tools: Vec<String>, unsupported_tools: Vec<String> } }
pub struct InstalledPack { id: PackId, version: Version, sha256: String, source: PackSource, enabled: bool, support: Support }
pub struct ConsentSummary { domains: Vec<String>, requires_env: Vec<EnvRequirement>, tools: Vec<String>, mutating_tools: Vec<String> }
pub struct NeoConsent { read_only_claims: Vec<String>, hinted_apps: Vec<String>, routines: Vec<RoutineSummary>, policies: PolicyDelta, support: Support }
pub struct TaskSnapshot { skills: Vec<SkillIndexLine>, routines: Vec<RoutineRef>, mode: Option<Mode>, tools: ToolIndex }   // immutable per task
pub struct Routine { name: String, description: String, examples: Vec<String>, params: Value, steps: Vec<RoutineStep>, verify: String, on_fail: OnFail, max_secs: u32 }
pub enum RoutineOutcome { Done { verify_p: f32 }, Handoff(HandoffBundle), Failed(String) }
pub struct PreparedRequest { method: Method, url: Url, display_url: String, headers: Vec<(String, Secret)>, body: Option<Value>, timeout: Duration }
pub trait SecretLookup { fn get(&self, pack: &PackId, name: &str) -> Option<Secret>; }   // checks the (pack, name) grant
pub trait StepRunner { async fn run(&self, tool: &str, args: Value) -> StepResult; }     // implemented by neo-agent over Gated<T>
```

## 11. UI and CLI

**Settings → Packs.** *Installed*: toggle, version, badges (*built in*, *partially supported*, *key missing* → opens `KeyField`, *update available*). *Pack detail*: skills (readable), tools (method, host, mutating), modes, hinted apps, **routines with steps and a "try it" box**, policies with provenance, consent summary, "not used by starkbot-neo", lock hash, rollback. *Browse*: registry search → manifest preview → install. *Updates*: consent diffs. *My pack* (`neo-user`): routine + hint editors with live validation. Mind pane lines: `loaded skill gtm-lead-research`, `routine neo-gtm/log_crm_activity (Jev 0.91) · params 410 ms · verify 0.88`, `pack tool octaweave_create_note → 201`.

**CLI.** `neo pack list [--json] | search <q> | add <git-url|@handle|path|file> [--enable] | remove | enable | disable | update [--all] | clone <id> [--edit] | validate <dir> | consent <id|path> | new <id> | lock verify` · `neo pack tool list|describe|call <name> --args '{…}'` · `neo routine list | match "<utterance>" | params <name> "<utterance>" | run <name> --param k=v [--dry-run]`.

## 12. Omarchy-class extension workflow

Omarchy's useful idea is not “plugins are crates.” Its shell discovers namespaced `manifest.json` + QML directories from a built-in path and a user path, then offers list/enable/disable, git add/update/remove, clone-a-built-in, live reload and a community catalog. Its third-party QML runs unsandboxed inside the long-lived shell process. Neo copies the workflow, **not** that execution boundary: an assistant process can reach keys, logged-in browser sessions and macOS Accessibility, so third-party native code never enters the app process.

### 12.1 Data packs — the default extension tier

The existing `.agentpack` format covers most extensions without code: skills, modes/personas, schema-validated HTTP tools, Jev-matched routines, native-app hints and tighten-only policies. It gets the Omarchy ergonomics:

- ids are namespaced within the shared slug grammar: `neo-` is reserved, registry publishers use `<publisher>-<name>`, and local clones use `local-<user>-<name>`;
- embedded packs and `~/Library/Application Support/com.starkbot.neo/packs/` are discovered through one registry API; `neo pack list --json` reports source, version, enabled state, kinds, support and grants;
- `add <git-url>` resolves and records an **exact commit**, validates in staging, derives consent, then atomically installs. A mutable branch or upstream HEAD is never the installed identity;
- update fetches to staging, shows source + content + capability/consent diff, requires approval, atomically swaps, and keeps the previous version for rollback; dirty local clones are never overwritten;
- `clone neo-browser --edit` copies the data surface to `local-<user>-browser`, records `parent` + `replaces`, enables the clone and disables the replaceable built-in in one transaction. `neo-desktop`, safety policy and native-tool ownership cannot be replaced;
- developer-mode local directories are watched and revalidated on save. A valid revision affects only new `TaskSnapshot`s; an invalid revision leaves the last valid one active and shows every validation error;
- remove disables first. Registry/git installs are deleted only after rollback data is retained; hand-authored directories move to a timestamped backup. Pack grants and Keychain values are separate—removing a pack revokes its grants but never silently deletes a shared key.

### 12.2 Code components — arbitrary logic, capability confined

When declarative tools are insufficient, a pack may carry `component/component.wasm` plus `component/manifest.json`. This is a separate post-v1 tier and does not change the portable agent-pack interpretation on other hosts. Components target the WASI component model (`wasm32-wasip2`) and run in the separate `neo-extension-host` process under Wasmtime; no dylib, executable, shell script, npm install hook or in-process plugin is accepted.

The component manifest has `schema_version`, namespaced `id`, semver `version`, `world`, SDK version range, entry points, and requested capabilities. The installer re-derives imports/exports from the component and rejects a manifest that understates them. Initial WIT worlds:

M10 ships a new database migration, not a change to schema v1: `components(pack_id, world, sdk_range, status, failures, last_failure_at)`, `component_grants(pack_id, capability, scope JSON, granted_at, revoked_at)` and `component_kv(pack_id, key, value BLOB, updated_at)`. The writer actor enforces grant changes and the 16 MiB per-component KV quota transactionally; enablement is impossible without a component row whose bytes match `neo.lock`.

| Capability | Component can do | Boundary |
|---|---|---|
| `tool-provider` | expose typed tools to the three fixed pack meta-tools | arguments/results capped and schema-validated; every effect is proposed back to `Gated<T>` |
| `task-hook` | inspect redacted task lifecycle events and return advice/metadata | cannot change route, completion, gates or confirmations |
| `http:<hosts>` | ask the host HTTP runner to call approved origins | no sockets; declared hosts, redirect/private-IP rules, masking and spend gates still apply |
| `pack-storage` | read/write its own versioned key/value namespace | no paths and no cross-pack access; quota 16 MiB by default |
| `canvas-importer` / `canvas-exporter` | transform bounded frame/artifact byte streams | no studio filesystem handle; host chooses input/output files |
| `ui-card` | provide static assets for a card/panel and exchange schema-validated messages | opaque-origin sandboxed iframe, no Tauri IPC, navigation, downloads or direct network |

There is **no** capability for raw Keychain access, arbitrary filesystem, environment variables, subprocesses, Apple Events, Accessibility, CDP, input events, Tauri commands, unrestricted network or loading another component. Secrets remain host-side handles and are inserted only by the existing approved HTTP/provider path. Browser and macOS actions are host requests expressed as ordinary tool proposals and pass the same deterministic rules, Jev heads, confirm cards, pacing and caps.

Each invocation has fuel, epoch deadline, memory/table/stack limits, request/response byte caps and bounded concurrency. A trap, timeout, protocol violation or host crash fails that call, records a redacted diagnostic and disables the component after three failures; the main app remains alive. Components cannot run at app startup unless the user explicitly granted a `background` entry point, and background calls get separate rate and spend caps.

### 12.3 Catalog, review and authoring

The catalog indexes immutable tuples `(id, version, content_sha256, source_commit)`, not mutable repository heads. Publication requires manifest validation, exact-commit build reproducibility metadata, capability extraction, license, source link and automated hostile-fixture scans; “verified” means those checks passed, not that the code is safe. Install and update always show requested capability changes, domains, storage, background work, UI surfaces and exact hash.

`neo extension new <id> --rust` scaffolds a Rust component crate, WIT bindings, a data-pack wrapper and fixture host; TypeScript/Go templates may follow when their component toolchains are stable. `neo extension dev <dir>` uses a developer-only local grant, watches/rebuilds outside the app, hot-swaps only after validation, and exposes structured logs without secrets. Production Starkbot never invokes a compiler or package manager.

The Packs UI keeps one mental model: **Data** (safe/default) or **Component** (sandboxed code) badge, source/hash, permissions, enable toggle, update diff, rollback, “clone and edit,” and a kill switch. The first-party catalog itself is just another signed registry; users may add registries with `verified-only` or `explicit` trust.

## 13. Tests (Rust only)

- **Conformance:** the five real packs in `axoniac-seeded-agent-packs/packs` and their `dist/*.agentpack` archives read, hash-verify and validate; consent output equals metalcraft-agent's `derive_consent` for the same bytes (golden JSON).
- **Hostile archives:** `..` paths, symlinks, zip bomb beyond 64 MiB declared-small, wrong hash, two presets, missing persona/skill/integration, untyped parameter, undeclared `$NAME`, reserved env name, redirecting registry, id in two registries.
- **Runner** (against a local `axum` fixture server): URL/query expansion, all four body mappings, `error_path`, host tricks (`userinfo@`, backslash), parameter-host without `allowed_hosts`, private-IP refusal, redirect refusal, masking of `secret_paths` and of credential echoes, `save` / `save_inline` paths confined. **Isolation:** pack A cannot expand pack B's name; ungranted `(pack, name)` fails; secrets absent from trace rows and `Debug` output.
- **`desktop/`:** hint files with forbidden keys rejected; policies merge is monotone — property test: for any pack policies, merged rules ⊇ base and thresholds never looser; loosening value → install error.
- **Routines:** head-option rendering; param extraction with a stub text helper (null handling, `ask`); step executor over a fake `StepRunner` proving every step passes through the gate; handoff bundle marks executed mutating steps; verify fail-closed on Jev outage.
- **Meta-tools + lock:** tool list identical before/after enabling a pack mid-task; schema errors round-trip; lock tamper → pack disabled; consent-diff goldens; rollback.
- **No-hints rule:** the native scenario suite runs under `--no-hints` in the same job.
- **Components:** WIT import/export extraction must equal the manifest; no ambient WASI sockets/directories/env/processes; fuel, epoch, memory, byte and concurrency limits; traps isolate to `neo-extension-host`; denied capabilities never reach a host call; tool effects still traverse a fake `Gated<T>`; iframe fixtures cannot reach Tauri IPC or the network; exact-hash install, capability-diff update, rollback and three-strike disable.

## 14. Milestones

| M | Packs work |
|---|---|
| M4 | intake request reserves the `routine` head (empty option list until M9); policies merge point exists in the rules layer |
| **M5** | **registry wiring**: `PackRegistry` + `TaskSnapshot`, embedded `neo-desktop` / `neo-browser` registered through it, `load_skill` + skill index for embedded skills, modes from embedded personas, native tools declared via `native_tools` |
| M6 | generic enablement flow + `key_help` + grants, first used by `neo-media` |
| **M9** | **full data packs + Omarchy-class manager**: bundle read/validate, HTTP runner + meta-tools + gating, install from dir / file / exact git commit / registry, `neo.lock`, consent + diff, enable/disable, clone/edit, developer hot reload, updates/rollback, routines (head, params, executor, verify, handoff), `desktop/policies`, Packs UI, CLI, catalog, `neo-gtm`, `neo-user` |
| **M10** | **component SDK**: `neo-extension-host`, Wasmtime component runtime, WIT SDK + Rust template, capability broker/consent/revocation, `tool-provider`, `task-hook`, pack storage, host HTTP, sandboxed UI cards, component catalog validation |
| M11 | `desktop/apps` hints consumed by `AxObserver`; `neo-apple-apps`, `neo-electron`; the no-hints gate |

*M9 done when:* `octaweave` installs from the registry unchanged and "add a note to Octaweave called X" works by voice through the meta-tools with a confirm-free GET path and a gated POST; `vercel-vgpu` installs and shows *partially supported*; `neo-gtm/log_crm_activity` runs with zero Sol calls; a pack with a parameter host and no `allowed_hosts`, and one with a loosening policy, are both refused.

*M10 done when:* an independently built Rust component installs by exact hash, exposes one typed tool and one UI card, persists only inside its quota, and requests one host-checked HTTP call; attempts to open a socket/path/env var, call Tauri, bypass `Gated<T>` or exceed fuel/memory/time fail without crashing the app. Updating it shows a capability diff and rollback restores the prior component.

## 15. Risks

| Risk | Mitigation |
|---|---|
| Routine head misfires (wrong recipe for a free-form ask) | 0.75 threshold + `none` option; every step still gated; `verify` + handoff; verdict log feeds `neo judge eval` |
| Most existing personas assume a shell → packs feel half-working | honest *partially supported* badge with the dropped list; skills + HTTP tools still deliver value; registry filter "works fully in starkbot-neo" |
| Meta-tool indirection costs Sol an extra call and hurts tool choice | `list_pack_tools` returns ready/mutating flags; a mode pre-scopes the list; skills name the exact tools to call |
| Credential exfiltration by a hostile pack | declared-names-only expansion, per-`(pack, name)` grants, reserved names, parsed-URL host check, no redirects, consent shows hosts per key |
| `desktop/` rejected by a future host or registry validator | spec §2 amendment reserving it (open item 5); conformance test builds a `desktop/` pack with `packctl` and reads it with the ported `Bundle::read` |
| Format drift between four runner copies | golden consent + runner tests on the real packs; extract the shared crate after M9 |
| Wasm component becomes an escape hatch around gates | no ambient WASI; narrow WIT imports only; actions are proposals handled by the main process through `Gated<T>`; component host has no Keychain/Tauri handle |
| Extension crashes or exhausts resources | separate host process, fuel + epoch + memory/table/stack/byte/concurrency limits, three-strike disable, app remains alive |
| Marketplace points at code different from review | immutable source commit + content hash are install identity; exact-byte verification before consent; update requires a new review/diff |
| “Clone built-in” replaces safety-critical behavior | `neo-desktop`, safety and native-tool ownership are non-replaceable; clone only redirects replaceable data packs and affects new task snapshots |
| Hints quietly become a dependency | `--no-hints` CI gate; hint schema cannot express selectors |
