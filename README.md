# Starkbot Neo

Starkbot Neo is a local-first desktop agent in active development, running on
macOS and, as of the Linux port, on Linux too. It drives a browser over CDP and
native apps over the platform accessibility API — macOS Accessibility, AT-SPI on
Linux — with the Jev navigator, a classifier that answers one operation head, one
target head and the safety heads per step, and puts that loop behind an agent that
takes work by voice or text. The product it is being built into is a go-to-market
marketing and media harness: SEO and GTM workflows on the web, and media made by
operating real media apps rather than by generating files itself. It is not a shell
or coding agent; there is no `bash` tool and no general file access.

Status, as observed in this tree rather than as planned:

- The M0 spikes S1–S7 are run and their numbers recorded in
  [`plans/spikes.md`](plans/spikes.md); S8 (media through apps) has not been
  run, because nothing downstream of it is being built yet.
- M1 (workspace, SQLite store, `neo` CLI, the ratatui terminal front end, the
  Tauri developer shell, the subscription bridges, `neo doctor`) is complete
  except the Apple signing identity, which is still waiting on the user
  (00-decisions, *Still open* #1) and is what M1 is formally blocked on.
- M3's web navigator runs, the accessibility path with it (`neo nav`,
  `neo app`), together with a slice of M5: a streaming ReAct agent loop over
  the `browse`, `app` and `ax` tools.
- M4's deterministic rules, Jev safety heads, confirmation queue and
  user-question cards are live in both interactive front ends. A tripped action
  pauses for approval instead of silently proceeding or ending the run.
- Voice is push-to-talk in the terminal and start/stop dictation in the GUI;
  transcripts are editable before sending. There is no speech out.
- Media through apps, packs and the GTM workflows are unbuilt.

## Install and launch from source

Starkbot Neo runs as a real Tauri desktop app on **macOS and Linux**. There
are no published installers or release downloads yet. After installing the
prerequisites below, use the same three commands on either platform:

```sh
git clone https://github.com/ethereumdegen/starkbot-neo.git
cd starkbot-neo
./run_gui.sh
```

The first launch installs the locked frontend dependencies with npm, installs
a pinned Tauri CLI **inside this checkout**, builds the release desktop with
its frontend embedded, and launches it. Allow several minutes, network access
for dependencies, and disk space for Rust build artifacts. No Vite server is
required. The launcher never installs system packages, runs `sudo`, installs a
global npm package, or copies an app into `/Applications`.

Run `./run_gui.sh` again to launch after closing the app or updating the source.
It reuses installed dependencies when the package manifests are unchanged,
rebuilds the frontend, and lets Cargo reuse unchanged Rust compilation work.
It does not guess build freshness from timestamps or launch an old binary
after a failed build. The terminal stays attached for diagnostics; **Ctrl-C**
stops the build or app. `./run_gui.sh --help` describes the launcher. It also
works by absolute path from another directory, including paths with spaces.

### Prerequisites

- **macOS** on Apple Silicon or Intel, or **Linux x86-64** with an active
  Wayland or X11 desktop session (not a headless server).
- **Git**, [Rust installed through rustup](https://rustup.rs), and
  [Node.js **22.12 or newer** with npm](https://nodejs.org). The repository's
  `rust-toolchain.toml` pins Rust **1.91.0**; rustup prepares that toolchain
  when the launcher first uses it. Reopen your terminal after installing tools
  so `git`, `rustup`, `cargo`, `rustc`, `node`, and `npm` are on `PATH`.
- For agent tasks, a Claude Pro/Max or eligible ChatGPT account, or an OpenAI
  API key for inference, plus a TypeSafe key for the navigator. Configure these
  in **Connections**; they are not needed just to build and open the window.
  Browser automation also needs Google Chrome.

#### macOS prerequisites

Install Apple's compiler and SDK tools:

```sh
xcode-select --install
```

Finish the installer (or use an existing Xcode installation), then run the
quick start above. The launcher builds `Starkbot Neo.app` using
`src-tauri/tauri.macos.conf.json`, including the microphone usage description
and audio-input entitlement. It ad-hoc signs local builds by default; no paid
Apple certificate is required. If you have a signing identity, set
`APPLE_SIGNING_IDENTITY` to use it. A source-built local app is not a notarized
release, and rebuilding an ad-hoc-signed app can cause permission or Keychain
prompts to return.

#### Linux prerequisites

Install the native compiler tools and desktop development libraries yourself.
For Debian/Ubuntu with WebKitGTK 4.1 packages available:

```sh
sudo apt update
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev \
  libgtk-3-dev libsoup-3.0-dev libasound2-dev \
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev
```

For Arch Linux:

```sh
sudo pacman -S --needed base-devel pkgconf webkit2gtk-4.1 gtk3 libsoup3 \
  alsa-lib libayatana-appindicator librsvg xdotool
```

For Fedora:

```sh
sudo dnf install gcc gcc-c++ make pkgconf-pkg-config webkit2gtk4.1-devel \
  gtk3-devel libsoup3-devel alsa-lib-devel \
  libayatana-appindicator-gtk3-devel librsvg2-devel libxdo-devel
```

These commands are manual prerequisites, not actions the launcher performs.
Other distributions need equivalent development packages. Run the launcher
inside your graphical desktop. It checks for the compiler tools and
GTK 3, WebKitGTK 4.1, libsoup 3 and ALSA via `pkg-config` before building.
For native-app automation, enable the desktop's AT-SPI accessibility service.
A Secret Service provider (GNOME Keyring, KWallet or KeePassXC) is recommended
for credential storage.

### First launch and permissions

- Open **Connections** to connect an inference account and configure navigator
  credentials. Connection or permission failures are also explained by
  `cargo run -- doctor`; see the command-line examples below.
- On macOS, allow **Microphone** when starting GUI dictation. For native-app
  control, grant **Accessibility** in System Settings → Privacy & Security
  to Starkbot Neo (or the terminal if macOS attributes a terminal-launched
  process to it). Use the permission diagnostic to check the actual requester.
- GUI dictation uses OpenAI `gpt-transcribe` on **both platforms**. Add an
  OpenAI key in **Connections** and select an available microphone. A Claude
  or ChatGPT subscription does not supply this transcription key. Transcripts
  are editable before sending; there is no speech output.
- On Linux NVIDIA/Wayland sessions, the launcher applies the same WebKit
  DMABUF workaround as `neo gui`, unless you explicitly set
  `WEBKIT_DISABLE_DMABUF_RENDERER` yourself.

If setup or building fails, fix the diagnostic printed in the terminal and
rerun `./run_gui.sh`; no desktop is launched on a failed build. To reinstall
frontend packages manually, use `npm --prefix ui ci --include=dev --include=optional`.
Builds live under `target/<native-rust-target>/release` (including
`bundle/macos/Starkbot Neo.app` on macOS). `CARGO_TARGET_DIR` is honored;
relative values are resolved from the directory where you invoke the script.

## Platform differences

The portable half — the store, the CLI, the TUI, the agent loop, the web
navigator over CDP — is the whole product on either platform. Three things
differ, and `neo doctor` reports each of them as a row rather than leaving
you to find out mid-task:

| | macOS | Linux |
| --- | --- | --- |
| Dictation | terminal can use on-device recognition; GUI uses OpenAI `gpt-transcribe` | OpenAI `gpt-transcribe`; requires an OpenAI key |
| `neo app` (native apps) | the Accessibility API | AT-SPI on the session bus; `neo doctor` reports the bus and whether it is enabled. A field that already holds text is the known gap: clearing it needs ⌃A, which a WebKitGTK window under Wayland reads as a bare `a` |
| Desktop shell | Tauri full window and floating mini mode, with Retina-aware positioning | WebKitGTK full window; floating mini mode on Hyprland or X11 |

Credentials go to the login Keychain on macOS and to whatever owns
`org.freedesktop.secrets` on Linux (gnome-keyring, KWallet, KeePassXC),
falling back to a `0600` file when the session has no keyring. Data lives
under `~/Library/Application Support/com.starkbot.neo` on macOS and the XDG
base directories (`~/.local/share/starkbot-neo` and friends) on Linux.

Building the desktop shell on Linux needs webkit2gtk 4.1, GTK 3, libsoup 3
and ALSA headers; `neo` itself needs none of them. Both `./run_gui.sh` and
`neo gui` set `WEBKIT_DISABLE_DMABUF_RENDERER` on an NVIDIA Wayland session,
where WebKit's DMABUF renderer commits a buffer with no acquire point and the
compositor closes the window a second after it opens.

## Run

Check the machine first: `neo doctor` names every connection, missing key and
platform capability — the macOS permissions, the Linux accessibility bus —
and exits non-zero when something has to be fixed.

```sh
cargo run -- doctor
cargo run -- account --provider anthropic-oauth login   # the Claude subscription
cargo run -- ask "Reply with exactly: bridge works"
```

The three front ends over the same runtime:

```sh
cargo run -- tui                      # terminal front end, full agent loop
./run_gui.sh                          # standalone desktop app, no dev server
cargo run -- nav https://example.com "click the More information link"
cargo run -- app com.apple.Numbers "pick the Blank template"
```

Projects keep standing context in `soul.md`, recurring work in
`heartbeat.md`, and an independent heartbeat clock (four hours by default):

```sh
cargo run -- projects add "Q4 launch"
cargo run -- projects edit q4-launch --soul
cargo run -- projects edit q4-launch --heartbeat
cargo run -- projects heartbeat q4-launch --on --every 4h
cargo run -- heartbeat run q4-launch
```

Managed projects live under the data directory. `--root /existing/folder`
attaches an existing folder without creating or scanning unrelated files.

`cargo run` is `neo`: the workspace's `default-members` names `crates/neo-cli`,
which is also why a bare `cargo build`, `cargo test` or `cargo clippy` covers
that package alone. Add `--workspace` for the whole thing, as CI does.

For desktop development with hot reload, `cargo run -- gui` runs
`cargo tauri dev` in the checkout. This separate developer workflow needs
the Cargo Tauri CLI (`cargo install tauri-cli --version 2.11.5 --locked`)
and frontend dependencies (`npm --prefix ui ci --include=dev --include=optional`).
It starts Vite on port 1420; a development desktop binary started alone
would show an empty window. `neo gui --build` builds instead. For normal
source installation and launching, prefer `./run_gui.sh`, which installs its
own local CLI and builds an embedded-frontend app rather than a dev window.

The sidebar's top toggle collapses navigation into labeled, tooltip-equipped
icons and expands it again. A focused chat uses the whole content area;
**‹ Threads** returns to the thread index, and selecting a thread hides
the index again. Returning to the index does not stop the current agent turn.

`neo nav` and `neo app` drive one surface directly through the navigator —
a web page over CDP, a native app over the accessibility path — with the same
policy, element budget and safety heads the agent loop uses. An app is named by
bundle id, pid, or a substring of its name, and is launched if it is not
running.

### Mini mode

The **Mini mode** pill is at the bottom of the GUI sidebar, directly above
the bridge version. It opens a dark, draggable 560×220 composer near the
bottom of the current display. **Full mode** restores the full window;
**Ctrl+Shift+M** (or **Cmd+Shift+M** on Mac) toggles either direction.
The conversation, in-flight agent turn, and unsent draft are shared.

- **Typing** accepts a text query. **Accept** sends it to the current
  conversation, or steers that conversation's running turn.
- **Microphone** starts recording and shows **Listening** with a live FFT.
  **Stop mic** sends the audio to OpenAI `gpt-transcribe` and appends the
  transcript to the editable query. Nothing is sent to the agent until
  **Accept**. Configure the OpenAI key in **Connections** on either OS.
- **Cancel mic** discards the recording. **Escape** cancels dictation, or
  stops the agent if no dictation is active. Recordings are limited to
  two minutes and are not written to disk.
- Pending approvals/questions remain explicit: **Review** expands the
  full conversation rather than treating Accept as an approval.

The same control is available to scripts and agents without clicking:

```sh
cargo run -- window mini
cargo run -- window full
cargo run -- window toggle
cargo run -- window status
```

These commands operate on the already-running GUI, honor `--data-dir`,
and print `{"ok":true,"mode":"mini"}` (or `"full"`) only after the GUI
acknowledges completion. The existing private `control.sock` JSON-lines
API accepts `{"window_mode":"mini"}` with the value `mini`, `full`,
`toggle`, or `status`, as an alternative to `{"say":"…"}`.
Errors/timeouts return `ok:false`;
they do not silently launch another window.

On MacBook, mini mode uses native AppKit window behavior through Tauri,
with the monitor's work area and scale accounting for Retina, the Dock,
and the menu bar. The macOS bundle includes the microphone usage
description and hardened-runtime audio-input entitlement. Allow
Microphone access when prompted; GUI dictation still requires the OpenAI
key, not Apple's on-device speech recognizer.

On Hyprland, the app moves only its own window through compositor IPC;
no desktop configuration is changed. Returning from mini restores the
previous floating/tiled state (the compositor controls tiled placement).
Other Wayland compositors currently report unsupported window placement.
Hyprland raises mini above other windows, but a subsequently focused
floating window can cover it.

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

- [`PLAN.md`](PLAN.md) — what the product is and who does what
- [`plans/00-decisions.md`](plans/00-decisions.md) — the decision record, including the authoritative milestone table
- [`plans/16-quality.md`](plans/16-quality.md) — the quality upgrade (Q0–Q5), which is the work currently in progress
- [`plans/`](plans/) — architecture and product contracts
- [`plans/08-providers.md`](plans/08-providers.md) — account, provider, and cost policy

## Status and safety

This is pre-release software. Review commands before using it against important repositories or accounts. Generated changes remain subject to the permissions and confirmation rules documented in the plans.

## License

Starkbot Neo is MIT-licensed. Downloaded Codex artifacts are Apache-2.0 and retain their own license.
