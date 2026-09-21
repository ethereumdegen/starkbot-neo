# 17 — Linux

Dated 2026-09-21. Constitution: P16, A36, A37, S8d ([00](00-decisions.md)).

Starkbot Neo runs on Linux. One product, one policy, one tool surface: the
platform is a backend choice inside two crates plus a path helper, never a
second behaviour. Everything Jev sees — the element table, the 250-element
budget, the pruning rules, the freshness guard, the deny list, the packs — is
platform-blind and stays that way.

Reference machine: Arch (Omarchy), Hyprland 0.56 on Wayland, `at-spi2-core
2.60`, `webkit2gtk-4.1 2.52`, `/usr/bin/chromium`, gnome-keyring's Secret
Service portal, `wtype`. No X11 session, no `xdotool`, no `ydotool`.

## 1. What is already portable

Measured, not assumed: the CI Linux lane
([16-remediation](16-remediation.md) R0.2) already builds and tests `neo-core`,
`neo-store`, `neo-keys`, `neo-cdp`, `neo-otel`, `neo-judge` and `jev-nav
--no-default-features` — 103 tests. The whole **web** half of the product,
which is focus (1) of P2, is portable Rust plus a CDP client plus
`snapshot.js`. `neo-tui` is `ratatui` + `crossterm`. Nothing in the agent loop,
the gates, the caps, the trace, the pack format or the routines is macOS-shaped.

What is not portable is exactly five things, and each has one Linux answer.

| | macOS | Linux |
|---|---|---|
| native accessibility | `AXUIElement` + `CGEvent` (`neo-ax`) | AT-SPI2 over D-Bus (§3) |
| secrets | login Keychain, `security-framework` | Secret Service, `org.freedesktop.secrets` (§4) |
| data locations | `~/Library/{Application Support,Logs,Caches}/com.starkbot.neo` | XDG (§5) |
| managed browser | `/Applications/Google Chrome.app/…` | Chromium-family on `PATH` (§2) |
| speech in | `SFSpeechRecognizer`, on-device, free | OpenAI `gpt-transcribe` only (§6) |

## 2. The web path (L1)

`neo-cdp` hardcodes `DEFAULT_CHROME` to the `/Applications` path
(`crates/neo-cdp/src/lib.rs:23`). It becomes `chrome_path() -> Option<PathBuf>`:
`$CHROME` / `$BROWSER` if they name a Chromium-family binary, then `PATH`
(`google-chrome-stable`, `google-chrome`, `chromium`, `chromium-browser`,
`brave-browser`, `microsoft-edge`), with the `/Applications` bundles kept ahead
of `PATH` on macOS.

One behavioural bug, not a portability nicety: the `TYPE_TEXT` select-all sends

```
{"type":"keyDown","key":"a","code":"KeyA","modifiers":META,"commands":["selectAll"]}
```

(`crates/neo-cdp/src/lib.rs:683`). `commands` is AppKit's editing-command
mechanism as Blink exposes it and `META` is ⌘, so both read as macOS-only.
**Measured on Chromium 152 / Linux, and the guess was wrong in an instructive
way** — the four-row matrix, typing `typed` into a field holding `old value`:

| dispatch | result |
|---|---|
| `Meta` + `commands:["selectAll"]` (the old code) | `typed` — works |
| `Meta` + `windowsVirtualKeyCode`, no `commands` | `old valuetyped` — fails |
| `Ctrl`, no virtual key code | `old valuetyped` — fails |
| `Ctrl` + `windowsVirtualKeyCode: 65` | `typed` — works |

Current Blink applies the `commands` array on Linux too, so the old code did
select all. The real defect the matrix exposes is the **missing
`windowsVirtualKeyCode`**: Blink's own key-binding table resolves SelectAll
from it, so a `Ctrl` accelerator without it silently does nothing — and a
select-all that silently does nothing makes typed text *append*, which the
navigator's guard cannot distinguish from a field that was already longer.
The fix is therefore both: `Ctrl` off macOS (what Blink's binding table and
any page-level `ctrlKey` handler expect — depending on `commands` means
depending on a mechanism specified for AppKit) **and** the virtual key code on
every platform. `crates/neo-cdp/tests/replace_text.rs` is the permanent
regression: it drives real Chromium over the pipe transport at a `data:` URL
and asserts the field equals `typed`, not `old valuetyped`.

Launch also gains `--ozone-platform-hint=auto`. Verified headed: `hyprctl
clients` reports `class="chromium" xwayland=false`, so Chromium runs
Wayland-native rather than through XWayland. `--no-sandbox` is **not** added —
every launch succeeded under the normal user-namespace sandbox, and adding it
speculatively would be a security regression.

`chrome_path()` replaces `DEFAULT_CHROME` outright. Order: `$CHROME`, then
`$BROWSER` (XDG colon list, `%s` stripped), then the `/Applications` bundles on
macOS, then `PATH` by name priority. Both env paths are accepted only if the
file name contains `chrom|brave|edge`, which matters on this machine: `$BROWSER`
is `omarchy-launch-browser`, a wrapper that would have swallowed
`--remote-debugging-pipe`.

**Gate:** `neo nav https://example.com "click the More information link"`
passes; `replace_text.rs` passes; **S8d-web** (A37) passes 4 of 5 against
`dpaint serve`.

## 3. The native path (L3)

`neo-ax` becomes two backends behind one API (A36). `backend::mac` is today's
code, moved. `backend::atspi` is new. `types.rs`, `mapping.rs`, `table.rs` and
`deny.rs` compile everywhere.

### 3.1 The mapping

`atspi` (0.30, pure Rust over `zbus`) speaks the protocol directly; no
`libatspi` FFI, no GObject.

The table below is **as built and measured**, not as first drafted; §3.6 records
each correction and the evidence for it.

| macOS | AT-SPI2 |
|---|---|
| `AXUIElementCopyMultipleAttributeValues` | named property reads only — `Accessible.GetRole` / `Name` / `Description`, `Value.CurrentValue`, `Text.GetText(0, -1)`, `Component.GetExtents`, `Accessible.GetChildren`. **Never `Properties.GetAll`**: it reads `Locale`, which aborts LibreOffice (§3.6) |
| `AXUIElementCopyActionNames` → `AXPress`/`AXConfirm`/`AXPick`/`AXShowMenu` | `Action.NActions` + `GetName(i)` → `DoAction(i)`. **Not `GetActions`**, which never returns on WebKitGTK and comes back empty on Chromium (§3.6). Names seen: `press` on VCL and WebKitGTK, `click` on Chromium |
| `AXUIElementSetAttributeValue(AXValue)` | `EditableText.SetTextContents`, `Value.SetCurrentValue` — where they exist. **WebKitGTK implements no `EditableText` at all**, so text into a Tauri window goes through `Component.GrabFocus` plus the virtual keyboard (§3.3), which makes the fallback the common path rather than the exception |
| `AXFocused` | `Component.GrabFocus` |
| `AXUIElementCopyElementAtPosition` | `Component.GetAccessibleAtPoint` |
| `AXSheet` / `AXDialog` subrole = modal | `Role::Dialog` / `Alert`, **with `State::Modal` as a second trigger, not a requirement** — WebKitGTK reports `aria-modal=true` dialogs without it. This matches macOS, where the subrole carries the meaning and no modal state exists |
| `AXProgressIndicator` / `AXBusyIndicator` | `Role::ProgressBar`, `State::Busy` |
| `AXMenuBar` → menu leaves with shortcuts | `Role::MenuBar`/`Menu`/`MenuItem`, accelerator from `Action.GetKeyBinding` |
| `CLICK_ROLES` / `TEXT_ROLES` / `SELECT_ROLES` (`mapping.rs`) | `canonical_role` expresses AT-SPI roles in those same sets — one policy, two vocabularies |
| `AXEnhancedUserInterface` / `AXManualAccessibility` | `ax_strategy` gains `force_renderer_accessibility` (Chromium/Electron: `--force-renderer-accessibility`, or `org.a11y.Status.IsEnabled = true`) and `qt_always_on` (`QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1`) |

Visibility pruning deserves its own line, because the obvious rule is wrong:
**do not gate the walk on `State::Showing`.** LibreOffice's
`DocumentSpreadsheet` and every WebKitGTK GTK wrapper omit it while plainly on
screen — a `Showing`-gated walk saw 4 of 150 nodes in a WebKitGTK window.

The two round-trip costs differ in kind, and the difference survived the
optimisation: macOS batches attributes in one IPC call (3774 → 422 calls),
AT-SPI **cannot batch attributes at all**. One call costs ~29 µs on the a11y
bus, so the budget is met by concurrency plus collection-time pruning — the
branch this section always allowed. Measured on LibreOffice Calc, 2029 nodes:
naive 373 ms / 12 174 calls → pooled 66 ms / 10 145 calls → shipped, with the
menu split out and extras fetched only for survivors, **13.9 ms / 1951 calls**
(152 ms end to end through `AxHandle::table`, against the macOS gate of
p50 < 150 ms). degen-paint's Studio: 61 nodes, 2.3 ms pooled, 8.9 ms end to
end. Full table in [spikes](spikes.md). The 250-element budget, the diversity
rule and the pruning policy were not changed to fit the transport.

### 3.2 Apps, windows and focus

There is no `NSWorkspace`. Three pieces:

- **Identity** is the Wayland `app_id`. For GTK and Tauri apps it equals the
  desktop-entry id and the macOS bundle id (`dev.degenpaint.studio`), which is
  why `AppSel::BundleId`, the deny list and every pack hint carry over
  unchanged. `AppSel::Name` matches the window title or the desktop entry's
  `Name`.
- **Launch** reads the `.desktop` entry (`$XDG_DATA_DIRS/applications`),
  takes its `Exec`, strips the field codes and spawns it directly. Not a
  shell, not `gtk-launch`.
- **Enumerate and focus** is a `WindowManager` trait: `clients()`,
  `focus(pid|address)`, `frontmost()`. First implementation is **Hyprland
  IPC** (`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`:
  `j/clients`, `j/activewindow`). Focusing is **by window address**, not by
  pid: Hyprland 0.56 replaced string dispatch with Lua, so
  `dispatch focuswindow pid:N` is a syntax error there, and even where the
  string form parses a `pid:` selector is accepted and does nothing. The
  backend sends `hl.dsp.focus({ window = "address:0x…" })` and falls back to
  the string form for older releases. A second compositor is one more impl; a
  session with neither is `AxError::NoWindowManager`, never a silent degrade.

### 3.3 Input fallback

AT-SPI actions first, exactly as on macOS. When an element advertises no
usable action, macOS posts a `CGEvent`. On Wayland the equivalent is
`zwp_virtual_keyboard_v1` (keys) and `zwlr_virtual_pointer_v1` (pointer),
both of which Hyprland implements, driven through `wayland-client` in-process.

`xdotool` is X11-only. `ydotool` needs a root daemon on `/dev/uinput`. Both
would also mean spawning a program to press a key, which is a shell in
everything but name (P3). Refused.

### 3.4 Permissions

None. There is no TCC, nothing to grant, no prompt to raise. The Doctor
replaces the three macOS rows with facts it can check: the a11y bus answers,
`org.a11y.Status.IsEnabled` is true (Chromium and Electron only publish a tree
when it is), the compositor offers the two virtual-input protocols, and a
Chromium-family binary exists.

The **deny list** keeps its meaning and changes its keys: `Alacritty`,
`kitty`, `foot`, `com.mitchellh.ghostty`, `org.wezfurlong.wezterm`,
`org.gnome.Terminal`, `org.kde.konsole`, `code`, `dev.zed.Zed`,
`org.gnome.seahorse.Application`, `org.kde.kwalletmanager5`, `jetbrains-*`.

### 3.5 What publishes a tree, and what does not

GTK 3/4 and Qt (with the flag) publish natively. **WebKitGTK** publishes the
DOM's accessibility tree — which is what a Tauri app's window is on Linux, and
therefore what degen-paint's Studio is. Verified against a controlled page and
against the Studio itself: `role=option` arrives as `ListItem` carrying the
option text as its name, `role=listbox` as `ListBox`, `role=progressbar` and
`<progress>` as `ProgressBar`, a labelled `<input>` as a named `Entry`, and the
document structure as `DocumentWeb`/`Landmark`/`Section`/`Heading`. So A37's
contract does reach the native path through the web path's own role names.

Two exceptions, both measured and both handled in §3.1: a
`role="dialog" aria-modal="true"` arrives as `Dialog` **without** `State::Modal`
(Chromium does set it for `<dialog>.showModal()`), and WebKitGTK implements
**no `EditableText` interface at all**, so no `<input>` in a Tauri window can be
written through AT-SPI — typing goes through focus plus the virtual keyboard.
Electron needs `--force-renderer-accessibility`. A window that publishes
nothing is reported as such, the way an app with no `AXChildren` is today.

**Gate:** `neo app dev.degenpaint.studio "open the command palette and run
raster.layer.add"` passes; `neo eval` passes for Chromium, LibreOffice Calc
and degen-paint; **S8d-native** passes 4 of 5.

### 3.6 What L3 actually found

Written after the backend landed. §3.1–§3.5 above were written from
documentation; where this section contradicts them, this section is the
measurement. Numbers and method in [spikes](spikes.md).

**The transport is cheaper than feared and the call count is worse.** One
round trip on the a11y bus costs ~29 µs, so a naive walk of a LibreOffice
Calc frame is 12,174 calls but only 373 ms. Issuing the five per-node reads
together with 32 nodes in flight takes the same frame to 66 ms; splitting
the menu bar out (1,447 of 2,029 nodes) and fetching extents, actions, text
and description **only for the nodes the role policy might keep** takes the
whole observation to 14 ms of walk and 151 ms end to end through
`AxHandle::table`. The budget is met by concurrency plus collection-time
pruning, which §3.1 allowed; the 250-row budget, the diversity rule and the
pruning policy are untouched.

**Three ways of asking are forbidden**, each learned the hard way:
`Properties.GetAll` on `Accessible` reads `Locale` and **aborts
LibreOffice** (uncaught `IllegalAccessibleComponentStateException`, SIGABRT,
reproduced four times); `Action.GetActions` **never returns on WebKitGTK**
and returns empty names on Chromium, so actions are read with `NActions` +
`GetName(i)`; and `Text.GetText(0, n)` past the end returns `""`, so it must
be `GetText(0, -1)`.

**`org.a11y.atspi.Cache.GetItems` is not the shortcut it looks like.** It
returns LibreOffice's entire 2,029-node tree in one call and **five items**
for a WebKitGTK window. Unusable as a walk; not used.

**Pruning on `State::Showing` is wrong.** LibreOffice's
`DocumentSpreadsheet` and every WebKitGTK GTK wrapper omit it while on
screen; a `Showing`-gated walk saw 4 nodes of a 150-node Aether window.

**§3.2's Hyprland dispatch line is out of date.** On 0.56.2 the socket
evaluates the payload as Lua, so `dispatch focuswindow pid:N` is a *syntax
error*; the working form is
`hl.dsp.focus({ window = "address:0x…" })`. `j/clients`, `j/activewindow`
and `j/monitors` are unchanged. The backend tries the Lua form first and
falls back to the string form for older releases, and focuses **by address**
— `pid:` selectors were accepted and did nothing.

**§3.5 is right about WebKitGTK, with one exception that matters.**
`role="option"` arrives as `ListItem`, `role="progressbar"` and `<progress>`
as `ProgressBar`, a labelled `<input>` as a named `Entry`, and
`role="dialog" aria-modal="true"` as `Dialog` — but **without
`State::Modal`**, which Chromium does set for `<dialog>.showModal()`.
Modality is therefore decided by `Role::Dialog | Role::Alert`, with
`State::Modal` as a second trigger rather than a requirement; that also
matches macOS, which keys on the `AXDialog` subrole and has no modal state
at all. The second exception: **WebKitGTK implements no `EditableText`
interface**, so no `<input>` in a Tauri window can be written to — text goes
in through `Component.GrabFocus` plus the virtual keyboard, which makes
§3.3's fallback the common path rather than the exotic one.

**Spreadsheets are half-solved.** A Calc grid is a `ManagesDescendants`
`Table` with no children; cells come from `Table.GetAccessibleAt`, are named
`A1`, `B1`, … and carry their text, exactly as macOS reports them. But every
cell except the selected one answers `GetExtents` with a zero rectangle, so
the element table offers **one** cell where macOS offers 92. That is a
LibreOffice defect and it is reported, not papered over.

**The deny list, the launcher and identity all hold.** Hyprland's `class` is
the desktop-entry id for every app checked (`org.gnome.Nautilus`,
`dev.degenpaint.studio`, `libreoffice-calc`, `chromium`), so
`AppSel::BundleId` carries over unchanged; `AxHandle::apps` already hides
the three `foot` terminals on this session.

## 4. Secrets (L2)

`neo-keys` gains a Secret Service backend beside `Backend::Login` and
`Backend::File`. Default collection, attributes `{service:
"com.starkbot.neo", account: <name>}` so `secret-tool lookup service
com.starkbot.neo account openai` finds it, label `Starkbot Neo — <account>`.
Selection on Linux: Secret Service when `org.freedesktop.secrets` is on the
session bus, else the existing owner-only `0600` file with a warning the
caller can surface.

The macOS error mapping is preserved by meaning, not by code: a locked
collection the user refuses to unlock is `INTERACTION_NOT_ALLOWED`, a
dismissed prompt is `USER_CANCELED`, a missing item is `NOT_FOUND`. Callers
must not need a `cfg`.

A30's debug-build rule has no Linux counterpart to fear — there is no modal
authorization dialog per item — but the `NEO_KEYCHAIN_BACKEND` override stays,
now taking `secret-service` as well.

## 5. Paths (L0)

One helper in `neo-core`, every hardcoded `~/Library/` string routed through
it.

| | macOS | Linux |
|---|---|---|
| data (db, packs, chrome profile, attachments, `soul.md`, `heartbeat.md`) | `~/Library/Application Support/com.starkbot.neo` | `$XDG_DATA_HOME` \| `~/.local/share/starkbot-neo` |
| config | same as data | `$XDG_CONFIG_HOME` \| `~/.config/starkbot-neo` |
| logs | `~/Library/Logs/com.starkbot.neo` | `$XDG_STATE_HOME` \| `~/.local/state/starkbot-neo` |
| cache | `~/Library/Caches/com.starkbot.neo` | `$XDG_CACHE_HOME` \| `~/.cache/starkbot-neo` |

`heartbeat.md` moves with the config home, so [15-heartbeat](15-heartbeat.md)'s
path is platform-resolved rather than literal.

## 6. Voice (L0)

`cpal` captures over PipeWire unchanged. There is no on-device transcriber, so
K8's free path is macOS-only: on Linux `dictation` requires an OpenAI key, and
the Doctor says exactly that instead of reporting a denied permission that
does not exist. TTS was already OpenAI-key-only on both platforms.

## 7. The desktop shell (L4, optional)

Tauri v2 builds on webkit2gtk. `macos-private-api`, `tauri-nspanel` and the
`PanelController` are macOS-only and get `cfg`-gated; on Linux the shell opens
ordinary windows and the pill, ring and quick-entry overlays do not ship in
v1. P12 already makes `neo tui` the acceptance surface, so nothing is blocked
on this.

## 8. Milestones

| L | Name | State |
|---|---|---|
| L0 | Builds and runs | **done** — Linux target added; `neo-ax` split into `backend::{mac,unsupported}` with the public API intact everywhere; XDG paths through one `neo-core` helper; voice and `src-tauri` cfg-gated; the Doctor's TCC rows replaced by the four Linux facts plus a credential-storage row. `cargo build --workspace` and `cargo test --workspace` green (440 tests), clippy `-D warnings` clean, `cargo deny` clean, CI's Linux job is the whole workspace |
| L1 | Web path | **done** — `chrome_path()`; the select-all fix (§2) with `crates/neo-cdp/tests/replace_text.rs` as the permanent regression; `--ozone-platform-hint=auto` verified Wayland-native. `neo nav`'s executor half proven end to end against real Chromium without an LLM. **S8d-web** still to run |
| L2 | Secrets | **done** — Secret Service backend round-tripped through `secret-tool`, including upsert and delete, plus the no-keyring fallback and its warning. `neo tui` on a subscription connection still needs a key on this box |
| L3 | Native path | **done** — `backend::atspi` (bus, walk, apps, wm, input, actor, perm), Hyprland IPC by window address, desktop-entry launch, the Linux deny list, in-process Wayland virtual input. `cargo test -p neo-ax` 63 pass, clippy clean. Drove LibreOffice Calc and degen-paint's Studio through the public API. `neo app` is blocked only on a TypeSafe key. `neo-eval` app detection by desktop entry and **S8d-native** still to run |
| L4 | Shell | not started; `neo tui` is the acceptance surface (P12), so nothing is blocked on it |

L0–L2 were mechanical, as predicted. L3 was the work, and starting with a
measurement paid for itself: most of §3.1's load-bearing decisions are
corrections to what the documentation implied, and one of them —
`Properties.GetAll` aborting LibreOffice — would otherwise have been a crash
in the user's editor rather than a bug in ours.

## 9. Risks

| Risk | Control |
|---|---|
| AT-SPI round-trip cost blows the per-step budget | measure first (§3.1) and record in `spikes.md`; prune at collection time if needed; the budget and the pruning policy are not changed to fit the transport |
| An app publishes no tree (Electron without the flag, a bare Wayland surface) | `ax_strategy` hints set the flag where a flag exists; otherwise the Doctor and the observation say "no accessibility tree", the same named failure macOS already has |
| Compositor lock-in through Hyprland IPC | `WindowManager` is a trait with one impl today; a session with no impl is reported, never silently degraded |
| Wayland has no global "frontmost application" the way macOS does | the compositor's active window is the answer, and A32's screen lease already serialises the one keyboard |
| Two platforms, one behaviour, drifting | `neo eval` cases run on both; the pack format, hints schema and routines are shared, and a hint that only makes sense on one platform is a field on that platform's strategy enum, not a fork |
| Distro variation (no Secret Service, no PipeWire, X11 session) | each is a Doctor row with a named fix; the file keychain and typed input already cover the degraded cases |
