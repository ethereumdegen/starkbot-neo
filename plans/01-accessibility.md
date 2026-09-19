# 01 — Accessibility layer (`neo-ax`) — native apps only

Crate `neo-ax`, milestone **M11**. Pure Rust, macOS only, **no Tauri and no `neo-*` dependency** (`jev-nav`'s `ax` feature depends on it, and `jev-nav` is publishable on its own). It is the only code in the product that touches the macOS Accessibility (AX) API or posts `CGEvent`s.

## Scope

| Target | Path | Owner |
|---|---|---|
| Chrome-family browsers (Chrome, Chromium, Brave, Edge, Arc, Vivaldi, Opera) | CDP + in-page snapshot (A4) | `jev-nav::web::CdpObserver` — **not this crate** |
| Native macOS apps (AppKit, SwiftUI, Catalyst) | AX tree | `neo-ax` |
| Electron apps (Slack, Discord, Notion, Figma desktop…) | AX tree after `AXManualAccessibility` | `neo-ax` |
| Safari, Firefox | AX tree — a **secondary** path; web work belongs in Stark's Chrome | `neo-ax` |

Consequences, all firm:

- `AXEnhancedUserInterface` is **never set on any app**. Its costs (breaks Rectangle/Magnet window positioning and animation, can replay stray keystrokes when the AX client disconnects) are avoided entirely. There is no set/clear bookkeeping and no crash marker.
- For a Chrome-family bundle id, `neo-ax` returns only what the browser exposes unprompted (window chrome) and `AxObserver::observe` fails with `use_cdp`. The router sends web goals to `CdpObserver`; file upload is `DOM.setFileInputFiles`, so the native open panel is never needed for the web path.
- No screenshots, no vision, no Screen Recording permission (P10). An app with no usable AX tree is `BLOCKED: opaque_app`.
- Native-app automation is not what milestones are ordered around (P2). M11 lands after the web navigator, judge, Sol and the media/canvas/extension work.

Two consumers:

1. **`jev-nav::ax::AxObserver`** (A5) — needs an **element table**, `execute`, and freshness **guards**. Same navigator policy as the web: whole goal in, one TypeSafe request per step.
2. **Sol's fine-grained fallback tools** (A8) — `snapshot / find / read / press / set_value / type_text / key / select_menu / scroll / wait_for`, each behind `Gated<T>` (03), used when the navigator returns `BLOCKED` in a native app.

Both sit on the same actor, the same pruned snapshot, the same refs.

## Bindings

| Need | Crate |
|---|---|
| `AXUIElement*`, `AXObserver*`, `AXIsProcessTrustedWithOptions`, `AXUIElementCopyMultipleAttributeValues`, `AXUIElementSetMessagingTimeout`, `AXUIElementCopyElementAtPosition`, `AXUIElementCopyActionNames` | **`objc2-application-services` 0.3.x** — generated from SDK headers, best maintained, full coverage. Free `unsafe` fns → we write the safe layer. |
| `CGEvent` creation/posting, `CGEventPostToPid`, `CGPreflightPostEventAccess`, `CGEventKeyboardSetUnicodeString` | `objc2-core-graphics` |
| CF types, `CFRunLoop`, run-loop sources | `objc2-core-foundation` |
| Running apps, frontmost app, activate, launch, open URL | `objc2-app-kit` (`NSWorkspace`, `NSRunningApplication`) |
| ObjC exception safety | `objc2` with the `exception` feature |
| `IsSecureEventInputEnabled` | Carbon HIToolbox, one hand-written `extern "C"` line |

**Rejected:** `accessibility` / `accessibility-sys` (no batch fetch, stale since 2025-03) · `cidre` (self-described research) · `axuielement` (too new, Swift bridge — read for ideas only) · `enigo` / `rdev` for input (no per-pid posting, no modifier guard we control).

## Threading: one AX actor

`AXUIElement` is not `Send`. All AX work runs on **one dedicated OS thread that owns a `CFRunLoop`** (needed for `AXObserver` run-loop sources anyway). Elements never leave that thread; the rest of the process holds plain ids.

```rust
#[derive(Clone)]
pub struct AxHandle { tx: std::sync::mpsc::Sender<AxCmd> }     // Clone + Send, called from tokio

enum AxCmd {
    Apps      { reply: Reply<Vec<AppInfo>> },
    Activate  { app: AppSel,                                   reply: Reply<AppInfo> },
    Snapshot  { app: AppSel, scope: Scope, opts: SnapshotOpts, reply: Reply<Snapshot> },
    Table     { app: AppSel, opts: TableOpts,                  reply: Reply<ElementTable> },
    Find      { app: AppSel, query: Query,                     reply: Reply<Vec<Hit>> },
    Read      { r: Ref,                                        reply: Reply<String> },
    Guard     { g: Guard,                                      reply: Reply<Freshness> },
    Act       { target: Option<Ref>, action: Action,           reply: Reply<ActOutcome> },
    Mark      {                                                reply: Reply<MarkId> },
    SettleDiff{ since: MarkId, settle: Settle,                 reply: Reply<Diff> },
    WaitFor   { cond: Condition, timeout: Duration,            reply: Reply<WaitOutcome> },
    Watch     { app: AppSel, sink: mpsc::Sender<AxEvent> },
    ReleaseAll,                                                // kill switch: key-ups for every modifier
    Shutdown,
}
type Reply<T> = tokio::sync::oneshot::Sender<Result<T, AxError>>;
```

- The thread runs `CFRunLoopRunInMode(default, 10 ms, true)` slices and drains the command channel between slices; a command never waits more than one slice.
- One command at a time. `Act` and `Guard` for the same step are sent as a pair and the actor rejects an `Act` whose guard is older than 250 ms.
- Each command body runs inside `std::panic::catch_unwind`; a panic fails that command, bumps the generation (all refs die) and the actor carries on. If the thread itself dies the handle respawns it once and reports `Health::ax = degraded`.
- `ReleaseAll` is also reachable through a lock-free path (an `AtomicBool` checked between run-loop slices and between typed chunks) so the kill switch is honoured within ~10 ms even mid-typing.

### ObjC exception safety

Misbehaving apps and the CF↔ObjC bridge can raise exceptions that would unwind across FFI and abort the process. Every cluster of AX calls (one node fetch, one action, one observer registration) is wrapped in `objc2::exception::catch`; a caught exception becomes `AxError::Exception { app, call }`, the node is skipped, the walk continues. The exception wrapper and the `catch_unwind` wrapper are the only two places `unsafe` results are turned into `Result`s.

### Messaging timeouts

- `AXUIElementSetMessagingTimeout(systemWide, 2.0)` at actor start (process-wide default; the OS default is far longer and one hung app would freeze the actor).
- Per-app element: `1.0 s` once an app has answered its first snapshot in under 200 ms.
- `kAXErrorCannotComplete` → one retry after 250 ms → `AxError::Unresponsive { app }`; the app is marked unresponsive for 5 s so a walk does not pay the timeout per node.
- Wall deadlines sit on top: 400 ms per snapshot, 150 ms per element table, 2 s per action including settle.

## Snapshot

### Walk

1. **Root** = application element for a pid → focused window (default scope) | all windows | menu bar | subtree by `Ref`. A sheet, dialog or open menu attached to the window is always included, and when one is present it is walked **first**.
2. **Per node, one `AXUIElementCopyMultipleAttributeValues` call** for:
   `AXRole, AXSubrole, AXRoleDescription, AXTitle, AXDescription, AXValue, AXPlaceholderValue, AXHelp, AXIdentifier, AXEnabled, AXFocused, AXSelected, AXExpanded, AXPosition, AXSize, AXChildren, AXDOMIdentifier, AXDOMClassList, AXURL`.
   Measured elsewhere on a large web tree: **3,774 IPC calls → 422; ~1.2 s → ~30 ms**. A **per-attribute fallback** is kept for apps that reject the bulk call (decided per app on first failure, remembered for the process lifetime).
3. **Secure fields**: if `AXRole`/`AXSubrole` is `AXSecureTextField`, `AXValue` is **dropped before it is stored anywhere** and, in the per-attribute fallback, never requested. No code path — snapshot, `read`, diff, fixture capture, log — can return a secure field's value. `read` on one returns `AxError::SecureField`.
4. **Prune**:
   - keep a node if it is **interactive** (has `AXPress`/`AXPick`/`AXShowMenu`, a settable value, or a role in the interactive set) **or carries text** (title / value / description);
   - **depth tunnelling**: anonymous `AXGroup` / `AXGenericElement` / `AXSplitGroup` / `AXLayoutArea` wrappers with no text and no actions cost 0 depth and are not emitted — a depth budget of 25 then reaches DOM depth 30+ in Electron and Safari;
   - drop nodes whose frame does not intersect the window (or enclosing `AXWebArea` / `AXScrollArea`) frame; use `AXVisibleChildren` / `AXVisibleRows` where they exist (tables, lists, outlines only);
   - drop scrollbars, splitters, grow areas, empty containers, decorative images, zero-size nodes;
   - collapse a container whose only kept child repeats its label.
5. **Caps**: 1,500 kept nodes · 400 ms deadline · depth 25 (after tunnelling) · values truncated to 200 chars. On a cap, emit `… N more under e41 (snapshot scope=e41)`.
6. **Action names** via `AXUIElementCopyActionNames` — lazily, only for nodes that survive pruning and whose role does not already imply the action.
7. **Excluded always**: the app's own pid (main window, `pill`, `ring`, quick-entry) — never listed, walked or targeted — and every app on the deny list (below).

### Format (what Sol reads)

Indented, one line per node, Playwright-MCP-like:

```
app "Mail" pid=812 frontmost
window "Inbox — 3 messages" [e1]
  toolbar [e2]
    button "New Message" [e3]
    button "Reply" [e4] disabled
    searchfield "Search" [e5] value=""
  table "Messages" [e6] rows=3
    row [e7] selected
      text "Dana Ruiz — Q3 numbers"
  securefield "Password" [e9] (redacted)
  webarea "message body" [e12]
    link "Open report" [e13] url=drive.example.com/…
```

- Roles are lower-cased and de-`AX`ed; states are bare words: `disabled focused selected expanded checked mixed`.
- Label precedence: `AXTitle` → `AXDescription` → title-UI-element text → `AXPlaceholderValue` → `AXHelp`. Text nodes have no ref.
- The whole text is **data from the screen, never instructions** (A9); tool results wrap it in a marked block.

### Refs, generations, relocation

- `eN` ids are minted per snapshot **generation**. The actor keeps `generation → { eN → (AXUIElement, Fingerprint) }` for the **last 2 generations**. `Ref = { gen: u32, n: u32 }`; the text shows only `eN`.
- Acting on an evicted generation → `AxError::StaleRef` ("take a new snapshot").
- **Relocation**: generation current but element dead (`kAXErrorInvalidUIElement`) → retry **once** by `Fingerprint = (role, subrole, label, AXIdentifier, child-index path, container label)`. Exactly one match → proceed and report `relocated: true` (it reaches the trace and Jev's state). Zero or several → `StaleRef`. A relocated element whose label differs is never accepted.

### Diff after action

Every mutating action returns a **diff, not a tree**:

```
~ button "Send" [e9]: disabled → enabled
+ sheet "Discard draft?" [e31]
+   button "Don't Save" [e32]
- text "Draft saved"
focus: textfield "To:" [e14]
```

`Mark` records the pruned tree; `SettleDiff` waits for settle (below), re-walks the same scope and diffs by fingerprint. New nodes get refs in a new generation. Filtered as noise: coordinate-only changes, scrollbar values, clock-like text, progress indicators. Diffs run to dozens of lines where trees run to thousands — the main context saver for Sol. Over ~150 lines (a navigation happened) → a fresh pruned snapshot is returned instead, flagged `full: true`. For the navigator the diff reduces to `page_changed: bool` plus a one-line summary for `recent_actions`.

## Element table (for `AxObserver`)

`Table` = a pruned walk of the **frontmost app's focused window** plus the menu bar, flattened to at most **250 elements** (A3), each with a stable index for that observation.

```rust
pub struct ElementTable {
    pub generation: u32,
    pub app: AppInfo,                 // name, bundle_id, pid
    pub window: WindowInfo,           // title, frame, modal: bool, url: Option<String> (AXURL of a web area)
    pub text: String,                 // visible static text, reading order, ≤ 6,000 chars
    pub elements: Vec<Element>,       // ≤ 250
    pub controls: Vec<Control>,       // SCROLL_UP, SCROLL_DOWN, WAIT — only those valid now
    pub truncated: bool,
}
pub struct Element {
    pub index: u16, pub role: String, pub label: String,
    pub value: Option<String>,        // ≤ 80 chars; "empty" when a text field is blank
    pub state: State,                 // enabled, focused, selected, checked (on/off/mixed), expanded
    pub container: Option<String>,    // nearest labelled ancestor: "toolbar", "sheet Discard draft?", "row 3"
    pub operations: Vec<Operation>,   // subset of CLICK, TYPE_TEXT, SELECT, MENU
    pub options: Vec<String>,         // SELECT only
    r: Ref, frame: Rect,              // private: never serialised to Jev
}
```

Rendered to the policy as `[7] button  Send`, `[8] textfield  To: · empty` (10). The model's answer is only ever an **index into this table** — never a selector, coordinate or script.

### AX → operation mapping

| Operation | Offered on | Executes as |
|---|---|---|
| `CLICK` | button, link, checkbox, radio, menu button, pop-up button, disclosure triangle, tab, cell/row with `AXPress`, image with `AXPress`, items of an **open** menu | `press` (below) |
| `TYPE_TEXT` | `AXTextField`, `AXTextArea`, `AXSearchField`, `AXComboBox`, editable web-area text — enabled and with a settable `AXValue` or focusable. **Never** `AXSecureTextField`. | `set_value`, verified by read-back |
| `SELECT` | controls whose options are enumerable **without opening anything**: radio group, tab group, segmented control, combo box with a list, table/outline rows, a pop-up button that exposes its `AXMenu` while closed *(verify: many AppKit pop-ups expose children only when open — those stay `CLICK` and their open menu becomes `CLICK` targets next step)* | set `AXSelected` / `AXSelectedRows` / `AXValue`, or `AXPress` on the option |
| `MENU` | enabled **leaf** menu-bar items; label = full path `File › Export › PDF…`, value = shortcut (`⌘S`). Read without opening menus. | `select_menu(path)` |
| `PRESS_KEY` | control, offered when an element is focused; fixed key set: Enter, Escape, Tab, Shift-Tab, Space, arrows. No modifier chords — those go through `MENU`, whose labels the rules layer can read. | `key` |
| `SCROLL` | controls `SCROLL_UP` / `SCROLL_DOWN` (the wire names 10 uses), scoped to the scroll area containing the last target, else the window's largest scroll area; offered only when it can scroll that way | `scroll`, 80 % of the visible height |
| `WAIT` | control, when the window shows a busy/progress indicator or the last settle timed out | 100 ms, then re-observe |
| `DONE` / `BLOCKED` | always | terminal; `BLOCKED` carries a reason |

Budget of 250: a modal sheet/dialog, if present, **replaces** the window's elements (nothing behind a modal is offered). Otherwise window elements in reading order, on-screen first, up to 170; menu leaves fill the rest (≥ 80), ranked by token overlap with the goal and then menu order *(verify cost: a full menu-bar walk is ~300 batched calls; cache per pid, re-read only `AXEnabled`)*. `truncated = true` when anything was cut.

Secure fields appear as `securefield` with **no operations** so Jev can see that a password is wanted and answer `BLOCKED` (login walls are never passed — A9).

### Guards

`observe()` hands out a `Guard` with every executable action: `{ generation, pid, window fingerprint, modal flag, element fingerprint, frame, enabled }`. `execute()` first runs `AxCmd::Guard`; all checks must pass:

1. **Generation current** and the target app not on the deny list (re-checked — the list can change mid-task).
2. **Element still valid**: one batched re-fetch of role / label / enabled / position / size. Dead → relocate once; label or role changed → stale. Disabled → stale. Frame moved by more than its own size → stale.
3. **App frontmost, window focused**: `NSWorkspace.frontmostApplication` pid matches; `AXFocusedWindow` fingerprint matches; no sheet/dialog has appeared that was not in the observation.
4. **Not occluded**: `AXUIElementCopyElementAtPosition(systemWide, centre)` must return the target, a descendant, or an ancestor within 8 `AXParent` hops inside the same window. Another pid there → `occluded`. The `ring` panel is hidden before the hit-test and the app's own pid is never a valid answer *(verify that click-through panels are skipped by the hit-test)*.
5. **Session unlocked** (P7 backstop): a `locked` flag fed by the platform layer (05); set → `AxError::ScreenLocked`. The worker owns pausing; this only guarantees no event is ever posted into a lock screen.

Any failure → `Freshness::Stale(reason)` → `jev-nav` re-observes and re-decides. **Mutations are never blindly retried**: if an action was dispatched and its outcome is unknown, the step is recorded as `interrupted` and the next decision is made from a fresh observation.

`ExecResult = { performed, method: Ax | CgEvent, relocated, page_changed, summary }`.

## Actions

Order of preference — **AX first, `CGEvent` only as fallback**:

| Action | 1st | Fallback |
|---|---|---|
| `press` | `AXPress` (or `AXConfirm` / `AXPick` / `AXShowMenu` by role) | `CGEvent` click at the frame centre after `AXScrollToVisible` and the occlusion hit-test |
| `set_value` | set `AXValue`, then `AXConfirm` if offered; read back and compare | focus + select-all + `type_text`; read back |
| `type_text` | `CGEventKeyboardSetUnicodeString`, UTF-16 chunks ≤ 20 units, surrogate pairs never split, kill flag checked between chunks | — |
| `key` | `CGEvent` key down/up with flags | — |
| `select_menu` | walk `AXMenuBar` by title path, `AXPress` each level; verify each level exists and is enabled before pressing | — |
| `scroll` | `AXScrollToVisible` (to an element) / set the scrollbar's `AXValue` | scroll-wheel `CGEvent` over the scroll area |
| `activate` / `launch` / `open_url` | `NSRunningApplication.activate`, `NSWorkspace` (`open_url`: http/https only) | — |

Input rules:

- **Activate first.** A backgrounded native app exposes little more than its menu bar; every task step begins with the target app frontmost, and **frontmost is verified before any HID-level event**. `CGEventPostToPid` is preferred where the app honours it; global posting only with the frontmost check just passed.
- **Modifier `Drop` guard**: a held modifier is an RAII value whose `Drop` posts the key-up; `ReleaseAll` posts key-ups for every modifier regardless of tracked state.
- **Secure input**: if `IsSecureEventInputEnabled()` → refuse to type or send keys, `AxError::SecureInput`, and say why.
- **Points, not pixels**: AX frames and `CGEvent` locations are both global **points**, top-left origin of the primary display — no conversion between them. The only conversion (to per-display AppKit coordinates for the `ring` overlay) lives in one function, `geometry::ax_to_panel`.
- `CGPreflightPostEventAccess()` at start; posting rides on the same Accessibility grant.
- No synthetic mouse movement, drag or hover in v1 — elements that only respond to those are `BLOCKED` material.

## Observers and settle

`wait_for` and settle are **notification-driven, not sleep-polling**. One `AXObserver` per target pid, its source added to the actor's run loop, registered for `AXFocusedUIElementChanged, AXFocusedWindowChanged, AXWindowCreated, AXUIElementDestroyed, AXValueChanged, AXTitleChanged, AXLayoutChanged, AXSheetCreated, AXMenuOpened, AXMenuClosed, AXLoadComplete` (web areas).

- **Settle** = no notification for **150 ms**, capped at **2 s** (Sol's tools). The navigator uses the short profile: 50 ms quiet, 300 ms cap, 200 ms for combo boxes waiting on their list.
- Conditions: `element(query) appears | disappears`, `window title contains`, `focused app is`, `web load complete`, `value of eN changes`.
- The element passed to an observer callback is valid **only inside the callback** — copy pid + notification name + fingerprint, send a plain message.
- Apps that emit no notifications (some Electron builds) fall back to re-walk polling at 100 ms inside the same caps; the app is remembered as `silent`.

## Making any app readable — generic ladder, optional hints

P9: **everything must work with no profile at all.** The ladder below runs for any unknown app:

1. Snapshot. If the focused window is larger than 200×200 pt and yields fewer than 5 kept nodes →
2. if the bundle contains `Electron Framework.framework`, or unconditionally as a harmless probe, set **`AXManualAccessibility = true`** on the app element (no window side effects; old builds return `AttributeUnsupported` → fall through); poll for children up to 2 s;
3. if the window contains an empty `AXWebArea` (Safari loads content lazily) → wait for `AXLoadComplete` or children, up to 2 s;
4. still empty → `opaque_app` → `BLOCKED`.

Chrome-family bundle ids skip the ladder (`use_cdp`). `AXManualAccessibility` is set back to `false` when the task ends.

**Hints** (pack `desktop/` folder, A11) only shortcut the ladder or tune timing. They never contain selectors, element names, scripts or flows, and CI runs the fixture suite **with hints disabled**.

| App class | Hint fields (all optional) | Effect if removed |
|---|---|---|
| Electron (Slack, Discord, Notion…) | `ax_manual: true`, `silent: true`, `settle_ms` | one failed snapshot + ≤ 2 s probe on first contact |
| Safari | `lazy_web_area: true` *(verify whether `AXManualAccessibility` on the web area helps)* | one extra wait |
| Firefox | none needed — enables itself when `AXRole` is read; first snapshot may be partial → `WAIT` | — |
| Any | `tab_switch: keys` (tab strips are partly opaque to AX → `⌘1…9` / Window menu) | navigator uses the Window `MENU` items |

Long web content in Safari/Firefox: act on the visible region and scroll; never snapshot a whole document.

## Deny list and rules enforced here

`neo-ax` takes an `AxPolicy { deny: Vec<BundleMatcher>, own_pid }` and refuses at the lowest level — `AxError::Denied { app }` from snapshot, table, find, read and act — so no caller can bypass it.

Default deny list (P3, A9):

- **Terminal-class**: `com.apple.Terminal`, `com.googlecode.iterm2`, `dev.warp.Warp-Stable`, `com.mitchellh.ghostty`, `net.kovidgoyal.kitty`, `org.alacritty`, `com.github.wez.wezterm`, `co.zeit.hyper`, `org.tabby`.
- **Editors with integrated terminals** (denied as whole apps — a terminal pane cannot be told apart reliably in AX *(verify)*): VS Code and forks (Cursor, Windsurf, VSCodium), Zed, JetBrains IDEs, Nova.
- **Arbitrary-power tools**: Script Editor, Automator, Shortcuts.
- **Secrets**: Keychain Access, Passwords, 1Password, Bitwarden, and System Settings › Privacy & Security.

The user can add entries. Removing a default entry is a Settings → Safety action with a typed confirmation; `soul.md` and packs can only tighten (P8, A11). The deny list also applies to `activate`/`launch` and to occlusion answers (a denied app in front = `occluded`, never read).

## Permissions and TCC

- `AXIsProcessTrustedWithOptions(prompt = true)` **once**, from onboarding; then poll `AXIsProcessTrusted()` every second while the onboarding/Doctor screen is open. Deep link: `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`.
- The grant is keyed on bundle id **+ code requirement**. Ad-hoc builds = cdhash → **every rebuild silently invalidates the grant while the toggle still shows ON**. Developer ID builds (P1) keep it across updates.
- Under `tauri dev` / `cargo run` the bare binary is attributed to the **launching terminal** — grant the terminal for development; test the real grant only with a signed `.app`.
- Accessibility is requested at **M1 onboarding** even though `neo-ax` lands in M11, because nothing else in the product can work around a missing grant later; Screen Recording is never requested (P10).
- **`neo doctor`** (and Settings → Doctor) reports: trusted yes/no · *toggle on but untrusted* (prints `tccutil reset Accessibility com.starkbot.neo`) · `CGPreflightPostEventAccess` · secure input currently on (and which pid holds it) · signing identity / ad-hoc warning · a live probe (snapshot Finder's menu bar, expect > 5 nodes).

## Testing seams

```rust
pub trait AxSource { fn fetch(&self, n: NodeId) -> RawNode; fn children(&self, n: NodeId) -> Vec<NodeId>; … }   // below the pruner
#[async_trait] pub trait UiBackend: Send + Sync {                                                                   // above the actor
    async fn apps(&self) -> Result<Vec<AppInfo>>;
    async fn snapshot(&self, app: AppSel, scope: Scope, o: SnapshotOpts) -> Result<Snapshot>;
    async fn table(&self, app: AppSel, o: TableOpts) -> Result<ElementTable>;
    async fn guard(&self, g: Guard) -> Result<Freshness>;
    async fn act(&self, target: Option<Ref>, a: Action) -> Result<ActOutcome>;
    async fn mark(&self) -> Result<MarkId>;
    async fn settle_and_diff(&self, since: MarkId, s: Settle) -> Result<Diff>;
    async fn wait_for(&self, c: Condition, t: Duration) -> Result<WaitOutcome>;
}
```

- `LiveSource` (real AX) and `FixtureSource` (a captured raw tree) implement `AxSource`; walk, prune, format, table and diff are pure functions over it.
- `MacBackend` (wraps `AxHandle`) and **`FakeBackend`** implement `UiBackend`. `FakeBackend` = scripted trees + scripted effects ("pressing `Send` removes the window, adds a sheet"), plus fault injection: dead element, occlusion, app not frontmost, secure input on, unresponsive app. `AxObserver`, `Gated<T>` and the Sol tools are written against `UiBackend` only, so `jev-nav` and `neo-agent` tests run with **no Mac UI at all**.

## Test plan (Rust only)

| Layer | What | How |
|---|---|---|
| Raw-tree fixtures | `neo ax capture --app Mail -o fixtures/mail-inbox.axraw.json` dumps the **unpruned** tree with every fetched attribute (secure values never present; `--scrub` replaces text with same-shape filler). Fixture set: Finder, Notes, Mail, System Settings, Slack (Electron), Safari article, a modal sheet, an open menu, a 5,000-row table. | checked into the repo |
| Pruning + format | golden tests (`insta`) of snapshot text per fixture; node count ≤ cap; tunnelling reaches a known deep node; off-screen rows dropped | `cargo test` |
| Element table | golden tables; operation mapping per role; modal replaces window; 250 cap + `truncated`; securefield has no operations; menu ranking by goal | `cargo test` |
| Refs / relocation / diff | generation eviction, unique-match relocation, ambiguous → stale, label change rejected; diff noise filters; >150 lines → full snapshot | `cargo test` |
| Guards + execute | `FakeBackend` fault injection: each guard failure → `Stale`, no action executed; interrupted mutation never retried | `cargo test` |
| Safety | property test: no API returns a secure field's value; deny-listed bundle → `Denied` from every command; own pid never listed | `proptest` |
| Input | UTF-16 chunking (emoji, ZWJ sequences, 20-unit boundary); modifier guard releases on panic | `cargo test` |
| Live (manual, signed build) | `neo scenario notes-create`, `finder-rename`, `mail-draft-no-send`, `slack-compose-no-send`, `safari-search`, `denied-terminal`; each asserts final state by an independent AX read, never by the model's `DONE` | pre-release |
| Bench | `neo ax bench` — batched vs per-attribute, walk time, table time, per app | recorded per release |

## CLI surface (`neo ax …`)

`apps` · `snapshot [--app X] [--scope window|all|menubar|eN] [--raw]` · `table [--app X] [--goal "…"] [--json]` · `find "send button"` · `read eN` · `press eN` · `set eN "text"` · `type "text"` · `key return` · `menu "File > Export > PDF…"` · `scroll eN up|down` · `wait "<condition>" [--timeout ms]` · `watch [--app X]` (stream observer events) · `capture --app X -o file [--scrub]` · `replay file` (prune + table from a fixture) · `bench` · `doctor`.

Mutating subcommands obey the deny list and print the guard result and the diff. `neo nav "<goal>" --app Notes` (10) runs the navigator over `AxObserver`.

## Prior art (read before building)

`erishen/ax-agent` (Rust + Tauri 2, closest match: index-path refs + relocation hints, 2 s timeout, 35-step budget) · `andelf/axcli` (Rust, CSS-like selectors `AXButton[title*=…]`, `CGEventPostToPid`) · `mediar-ai` MacosUseSDK (diff-after-action) · Ghost OS (depth tunnelling, sticky modifiers, focus requirement) · Playwright MCP snapshot spec (ref lifecycle, element descriptions) · `browser-use/jev-ultrafast` `browser.py` guards (the freshness model ported here; read, never executed).

## Acceptance criteria — M11

1. `neo ax snapshot` of Mail, Notes, Finder and Slack: ≤ 1,500 nodes, p50 < 150 ms, p95 < 400 ms on Apple Silicon; batched fetch ≥ 5× fewer IPC calls than per-attribute (`bench`).
2. `neo ax table` p50 < 100 ms; every fixture's golden table stable; ≤ 250 elements.
3. `neo nav` completes, independently verified, with **zero Sol calls**: create a note with given title/body in Notes; rename a file in Finder; draft (not send) a Mail message; compose (not send) in Slack; all **with hints removed**.
4. Pressing `Send` in Mail raises a confirm card via the rules layer before anything executes.
5. A task aimed at Terminal, VS Code or Keychain Access ends `denied` without a single AX read of that app; the app's own windows never appear in any output.
6. A secure field's value is absent from snapshots, diffs, fixtures, logs and traces (property test + manual check with a filled password field).
7. Each guard failure (dead element, not frontmost, occluded, new sheet, locked) → re-observe, no action posted; kill switch mid-`type_text` stops within 50 ms and leaves no modifier down.
8. No call to `AXEnhancedUserInterface` exists in the workspace (CI grep); Chrome-family → `use_cdp`.
9. Sol's fallback tools pass the `FakeBackend` loop tests: snapshot → act by ref → diff; stale ref → structured error; relocation flagged.
10. `neo doctor` detects *toggle on, untrusted* on an ad-hoc rebuild.

## Risks

| Risk | Mitigation |
|---|---|
| Apps with thin or wrong AX trees (custom-drawn, games, some Catalyst/SwiftUI views) | `opaque_app` → `BLOCKED`, said plainly; no vision fallback by decision (P10) |
| Element table of 250 cannot hold a large window + menus | modal-first, on-screen-first, goal-ranked menus, `truncated` + scroll; measure in M11 before tuning |
| Menu-bar walk cost per step | per-pid cache, re-read `AXEnabled` only; drop to top-level `MENU` targets if over budget |
| `CGEvent` fallback acts on the wrong thing | frontmost + occlusion guards, AX-first ordering, no coordinates from the model |
| Whole-editor deny is blunt (blocks VS Code entirely) | documented; the user may move an editor to per-app mode *confirm everything* (typed confirmation); whether terminal-class entries are removable at all is an open question for the user |
| Ad-hoc dev builds lose TCC every rebuild | dev under a granted terminal; `neo doctor` explains; signing identity is an open item (00 §Still open 1) |
| Hung target app stalls the single actor | messaging timeouts, unresponsive marking, wall deadlines |
| Electron builds ignoring `AXManualAccessibility` | ladder step 4 → `BLOCKED`; *(verify coverage on current Slack/Discord/Notion)* |
| Secure-input left on by another app blocks typing | detected, named in the error and in Doctor |
