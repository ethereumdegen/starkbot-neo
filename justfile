# Contributor commands. Every recipe here is a command that works in this
# checkout today; nothing aspirational, so `just --list` can be read as the
# answer to "what can I run?".
#
# `cargo run` is `neo`: `default-members` names `crates/neo-cli`, which is why
# the cargo recipes below pass `--workspace` explicitly. just lists the comment
# line directly above a recipe, so the longer rationale sits above a blank line.

default:
    @just --list

# Format and lint exactly as CI's rust job does, in its order and flags.
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Every crate's tests, as CI runs them.
test:
    cargo test --workspace

# The members CI's linux job leaves out: Tauri needs webkit2gtk there and the
# desktop shell on Linux is deferred (plan 16 §8.1), and s6-panel/s7-webview
# are macOS-only spikes. Kept in one place so the recipe below and
# .github/workflows/ci.yml cannot drift apart silently.
linux_excludes := "--exclude neo-desktop --exclude s6-panel --exclude s7-webview"

# On a Linux box this *is* CI's linux job, command for command. On macOS it
# still runs — it just proves the exclude list names real members and that the
# remaining crates build; only the Ubuntu lane can prove the cfg gates.

# Lint and test the crate set CI's linux job builds.
linux:
    cargo clippy --workspace {{ linux_excludes }} --all-targets -- -D warnings
    cargo test --workspace {{ linux_excludes }}

# Per P12 this is the surface product behaviour is proven on first.

# Run the terminal front end, with the full agent loop.
tui:
    cargo run -- tui

# Run this before reporting that a run failed: it names the thing to fix.

# Check the machine: every connection, permission and missing key.
doctor:
    cargo run -- doctor

# `npm ci` is left out of what CI's ui job runs, because locally the
# checkout's node_modules is already what the lockfile says.

# Type-check and test the webview.
ui-test:
    cd ui && npx tsc --noEmit && npm test

# The same test fails in CI when the committed file has drifted.

# Regenerate the TypeScript bridge after changing a type the webview sees.
bindings:
    UPDATE_BINDINGS=1 cargo test -p neo-desktop bindings

# `always` rewrites the .snap files in place instead of leaving .snap.new
# beside them, so read the diff before committing: this golden is the guard
# that would otherwise have caught the change.

# Accept the TUI goldens after an intentional render change.
snapshots:
    INSTA_UPDATE=always cargo test -p neo-tui

# Needs the apps installed and a provider logged in, so it is not in `test`.

# Run the app-control eval suite: the agent driving real applications.
eval:
    cargo run -- eval
