# M0 spike findings

Initial results recorded 2026-09-18; browser-parity results updated 2026-09-19. These are measured prototype results, not release benchmarks. Each result names what actually ran; unrun acceptance criteria remain open.

## S1 — managed Chrome, CDP snapshot, Jev loop

Command: `s1-nav` against the local `spikes/fixtures/hotels.html` fixture with the goal “Find and open Casa Flora in Lisbon with Design style and free cancellation”, safety heads enabled, headless Chrome, and live TypeSafe/OpenAI calls.

| Measurement | Result |
|---|---:|
| Chrome launch + CDP connect | 2,267 ms |
| New tab + fixture navigation | 333 ms |
| Atomic snapshot, median of 5 | 1.0 ms |
| Initial action space | 8 actions / 4 elements / 0 omitted |
| Jev requests | 6 |
| Median Jev latency | 203 ms |
| Executed actions | 5 |
| CDP protocol calls during the loop | 33 |
| Whole navigator loop | 2,259 ms |

Observed sequence: `TYPE_TEXT Destination` → `SELECT Design` → `CLICK Free cancellation` → `CLICK Search` → `CLICK Casa Flora` → `DONE`. Every safety-head probability stayed below the 0.40 confirm threshold. Independent fixture evidence at exit was the page title `Casa Flora — Stayfinder`.

The first authenticated run exposed a real text-helper failure: it entered `Casa Flora, Lisbon` into the Destination field, producing zero results and a `BLOCKED` outcome. `TEXT_VALUE` now requires the helper to isolate the constraint owned by the selected field and not fold a named result into a location field. The repeated end-to-end run entered `Lisbon` and completed.

Conclusion at this stage: the minimal Rust CDP → snapshot → one multi-head TypeSafe request per step → guarded action loop worked on the representative local fixture. The 2.5-second hotel-fixture target was met by 241 ms in this run. The expanded parity spike below supersedes the earlier persistent-profile and live-site limitations.

### Browser parity extension

`s1-nav --bin parity` now runs a deterministic Chrome fixture suite with independent DOM assertions and no model calls. One run covered:

- an open shadow-root text field;
- a cross-site iframe (`127.0.0.1` parent → `localhost` child), proven by Chrome as an out-of-process `iframe` target, attached through its own flat CDP session, observed in its own isolated world, and clicked using top-level coordinates;
- a `contenteditable=\"plaintext-only\"` composer with multiline Shift+Enter insertion and read-back;
- a hidden file input populated through `DOM.setFileInputFiles`;
- a nested scrolling container;
- a focused-control `PRESS_KEY` operation;
- a `target=_blank` popup adopted only because its `openerId` belongs to the owned tab;
- a headed Chrome relaunch using the same non-default profile, with `localStorage` surviving the restart.

| Deterministic parity measurement | Result |
|---|---:|
| Full capability suite | PASS |
| Protocol calls | 132 |
| Suite runtime including first launch | 2,930 ms |
| Headed persistent-profile relaunch | 804 ms |

The initial run above used two ports on one site and an ephemeral WebSocket port. The transport/OOPIF extension below supersedes both limitations.

### CDP pipe and OOPIF extension

Managed Chrome now defaults to `--remote-debugging-pipe`. Rust maps parent pipe ends to Chrome file descriptors 3 and 4, writes and reads null-delimited CDP JSON, and keeps WebSocket as an explicit comparison/attachment mode. The public `Browser`/`Page` call surface is unchanged.

The navigator now discovers direct OOPIF targets omitted from the root page's `Page.getFrameTree`, attaches a flat session, enables Page/Runtime, creates the isolated world in that session, and routes every execution-context call—including file input resolution—through the owning session. The fixture asserts that Chrome reports the `localhost` child as an `iframe` target before accepting the run.

| Final parity sample | Pipe | WebSocket |
|---|---:|---:|
| Full suite protocol calls | 153 | 153 |
| Full suite including launch | 2,207 ms | 2,950 ms |
| Headed persistent-profile relaunch | 607 ms | 819 ms |

Three full-suite samples were noisy. Median whole-suite time was 1,680 ms over pipe versus 1,786 ms over WebSocket; median headed relaunch was 690 ms versus 757 ms.

A separate warm `Browser.getVersion` probe ran 200 sequential calls per transport across three Chrome launches:

| Transport measurement, median of three runs | Pipe | WebSocket |
|---|---:|---:|
| Launch → first successful CDP reply | 404 ms | 410 ms |
| Per-call median | 80 µs | 127 µs |
| Per-run p95, then median | 3,682 µs | 2,554 µs |

Decision: ship the pipe as the managed-browser default. It removes the listening debugging port and improves median call latency, but it did not improve tail latency in this small sample; no p95 performance claim is justified.

Authenticated live Wikipedia: “Open Gödel's incompleteness theorems” completed at the exact article URL and title in 2,934 ms, with 5 Jev requests, 2 executed actions, 32 protocol calls, and 229 ms median Jev latency. It met the ≤3.5 s whole-loop target but missed the ≤200 ms Jev median target. The run also reproduced and fixed isolated-world context invalidation across navigation (`Cannot find context with specified id`): a destroyed context is now treated as stale, rebuilt, and re-observed without retrying the mutation.

The hotel scenario still completed after the parity additions, with the exact property title and all requested filters. A later run took 2,920 ms, so the earlier 2.5 s result is not stable enough to call a latency pass; Luna and Jev network variance dominate the small CDP cost.

## S2 — everyday Chrome attachment

Decision: **do not ship everyday-Chrome attachment.** Installed Chrome 153 had no debugging endpoint on the conventional local port. More importantly, Chrome's official policy has ignored `--remote-debugging-port` and `--remote-debugging-pipe` against the default Chrome data directory since Chrome 136; both require a non-standard `--user-data-dir`. That makes a supported attachment to the user's normal profile unavailable. Stark's managed, non-default profile is the only browser path.

## S3 — Luna text helper

Live `gpt-5.6-luna` calls with reasoning disabled returned the correct field value `Lisbon` after the prompt fix.

| Sample | Latency | Tokens |
|---|---:|---:|
| Standalone helper probe before the prompt fix | 1,012 ms | 151 |
| Successful S1 run after the prompt fix | 643 ms | not printed by `s1-nav` |

Conclusion: output shape and semantic isolation work, but both observed calls exceeded the ≤600 ms target. Two samples are not a latency distribution; p50/p95 measurement remains open.

## S4 — Earshot to GPT-Transcribe

Command: `cargo run --release -p s4-voice`. The spike captured 30 seconds from the built-in microphone through CPAL's real callback into an `rtrb` SPSC ring, downmixed and resampled 48 kHz → 16 kHz through Rubato 5, ran Earshot over 256-sample frames, and sent a detected 5.331-second speech fixture to live `gpt-transcribe` five times with `prompt`, `keywords[]`, and `languages[]`.

| Measurement | Result |
|---|---:|
| Callback ring overflow | 0 |
| Real room input | 480,086 samples; −50.6 dBFS peak; −62.8 dBFS RMS |
| Normal-room false uploads | 0 in 30 s (0.00/min observed; not a long-run rate claim) |
| Earshot CPU for 30 s | 12 ms |
| Fixture detection | 1 segment; peak score 0.981 |
| Keyword transcription accuracy | 5/5 exact for `Stark`, `Hypercanvas`, and `autumn` |
| WAV encode | 2–8 ms |
| GPT-Transcribe request p50 / p95 | 810 / 1,299 ms |
| End-of-speech → final transcript p50 / p95 | 1,362 / 1,845 ms |

The original 700 ms hangover missed the p50 target in two samples; one also missed p95. Applying the predeclared first lever—34 frames = 544 ms, documented as 550 ms—brought the same five-request scenario under the 1.5 s / 2.5 s targets without speculative upload. The exact transcript returned on all five calls. Multipart arrays must use repeated `keywords[]` and `languages[]` parts; JSON strings are rejected with HTTP 400.

Decision: keep Earshot and batch `gpt-transcribe` as the default, change the normal hangover to 550 ms, retain the 350 ms short-command hangover, and keep `gpt-live-transcribe` optional. This proves the local pipeline and one quiet-room sample, not the M2 ten-minute music/silence, microphone-device, or one-hour soak criteria. Machine-readable evidence is `spikes/out/s4-voice/report.json`.

## S5 — metalcraft + Sol reasoning replay

Command: `cargo run -p s5-sol`. The spike uses local `metalcraft` 0.12 revision `3f5e865eae51186882ff20604dbbf658f12d24af` with `rig-core` 0.42 and OpenAI Responses. `gpt-5.6-sol` was forced through ten sequential `advance` tool calls and one terminal `finish` call. Every request used `parallel_tool_calls:false`, `store:false`, `include:["reasoning.encrypted_content"]`, `reasoning.summary:"auto"`, and a stable `prompt_cache_key`.

| Check | Observed result |
|---|---|
| Tool loop | 10/10 ordered `advance` calls plus `finish`; 11 model calls total |
| Reasoning retention | 5 encrypted reasoning items retained in `AgentState` |
| Reasoning replay | per-request replay counts `0, 0, 4, 5, 5, 5, 5, 5, 5, 5, 5`; no Responses validation error |
| Reasoning summaries | none returned at `low` effort despite `summary:"auto"`; the hook and storage path remained empty |
| Usage | 6,483 input, 386 output, 199 reasoning, 0 cached-input tokens |
| Wall time | 20,192 ms |

Decision: ship `rig` 0.42 and manual encrypted-item replay as the M5 reasoning spine. Keep `store:false` and explicit encrypted-content inclusion. Treat reasoning summaries as optional display data, not a Mind-pane or correctness dependency; this run proves the transport but `gpt-5.6-sol` did not emit summaries at low effort. Machine-readable evidence is `spikes/out/s5-sol/report.json`. The local path dependency is replaced only after the upgrade is committed and tagged exactly `v0.12.0`.

## S6 — non-activating NSPanel

Command: `cargo run -p s6-panel`. The spike is a real Tauri 2.11 application using `tauri-nspanel` at pinned revision `c9ec2130422200f0863b23dfdad02b133a529b07`. It launched managed Chrome through the production `neo-cdp` pipe transport, put Chrome in macOS fullscreen, and exercised the panels with real HID mouse and keyboard events.

| Check | Observed result |
|---|---|
| Passive surfaces above fullscreen Chrome | pill visible at level 101; ring visible at level 25 |
| Focus after showing pill + ring | Chrome remained frontmost; neither panel was key or capable of becoming key |
| Ring click-through | a real system click crossed the full-display ring and incremented Chrome's fixture counter |
| Clickable pill | a real system click reached the pill; Chrome remained frontmost and the pill remained non-key |
| Quick entry | the dedicated key-capable panel became key under `Accessory`, received `neo`, hid, restored `Prohibited`, and returned focus to Chrome |
| Lifecycle | hide/show passed; after a `Regular → Prohibited` policy cycle the passive panels were explicitly re-shown and remained healthy |
| Permission-free evidence | native `WKWebView.takeSnapshot` captures of `pill` and `ring` succeeded; no desktop capture or Screen Recording permission was used |

The machine-readable result is `spikes/out/s6-panel/report.json`; the native surface captures are `pill.png` and `ring.png` beside it. One measured run passed all 12 checks in 10,173 ms.

Decision: ship this shape. `pill` and `ring` use a panel subclass whose `canBecomeKeyWindow` is always false, borderless `nonactivatingPanel`, `fullScreenAuxiliary + canJoinAllSpaces + stationary + ignoresCycle`, `hidesOnDeactivate(false)`, and `Prohibited` activation policy while no normal window is open. The ring additionally ignores mouse events. `quick-entry` is a separate subclass that can become key; opening it switches to `Accessory`, and closing it restores the previous application and `Prohibited`. `main` or `design` uses `Regular`.

Important lifecycle finding: changing activation policy can order passive panels out. The shell's panel controller must treat every policy transition as a visibility transition and explicitly re-show the surfaces that should remain visible. Panel snapshots for regression evidence must use the owned WKWebViews' snapshot API, never `screencapture`.

## S7 — canvas rendering

The Rust/CDP renderer produced and the visual check inspected these artifacts:

| Artifact | Observed result |
|---|---|
| `frame@1x.png` | 1080 × 1080 RGB PNG |
| `frame@2x.png` | 2160 × 2160 RGB PNG |
| `frame@3x.png` | 3240 × 3240 RGB PNG |
| `frame.mp4` | 1080 × 1080, H.264, 30 fps, 2.0 s, no audio, 188.4 KiB |

The inspected frame had no unintended clipping or rendering failure; the orb crossing the right/bottom edges is intentional CSS overflow. The video visibly advances the rise and spin animations across sampled frames.

Conclusion: CDP emits exact-scale stills and a playable frame-stepped MP4. The native WKWebView path now captures its Retina backing image at 2160 × 2160 and normalizes it to the canonical 1080 × 1080 fixture size before comparison.

### Interactive Hypercanvas extension

`s7-render --bin canvas` now exercises a real canvas surface in Chrome rather than only exporting a frame. The surface uses a scriptless `sandbox=\"allow-same-origin\"` frame document, a screen-space input overlay, and a separate Rust document model. Pointer and wheel events become intents; Rust validates and commits transactions; DOM patches return to the surface.

One release-mode run exercised element hit-testing and selection, drag, resize, double-click text edit, zero-round-trip knob preview followed by one commit, an element-anchored pin, space-tool pan, cursor-centred zoom, marquee, stable-id agent copy rewrite, conflict-aware per-author undo, and isolated 1080 × 1080 PNG export.

| Interactive measurement | Result |
|---|---:|
| Whole scripted interaction scenario | PASS in 2,329 ms |
| Rust transactions / final revision | 8 / 8 |
| Slowest validate + inverse + commit | 53 µs |
| Knob preview + browser layout | 2,983 µs |
| Canonical 1080² Chrome export | 111 ms |
| CDP protocol calls | 101 |

The visual inspection confirmed `hypercanvas-surface.png` shows the panned/zoomed frame, edited headline, knob state, and anchored pin without clipping; `hypercanvas-export.png` is the clean frame without editor chrome. The transaction checks prove byte-identical simple undo and the property conflict rule: undoing an agent rewrite restores its CTA change but preserves a later user headline edit. The first implementation treated the whole rewrite inverse as one footprint; the check exposed that error and the inverse now filters conflicts per changed node.

This is a Figma/Canva/Claude-Design-style interaction proof, not M7. It does not include Tauri channels, virtualisation, reconciliation of missing/duplicate ids, sanitisation, Jev voice micro-edits, or production exports. This run used Chrome only; the separate Tauri/WKWebView comparison below covers engine drift and sandbox patching.

### Tauri WKWebView engine-drift extension

`s7-webview` is a real Tauri 2.11 application using Wry/WKWebView. It loaded each frame as `srcdoc` in a scriptless `sandbox=\"allow-same-origin\"` iframe. The parent Tauri page patched a `data-n` element and measured every node box. `s7-render --bin engine_drift` loaded the same source fixtures and the same measurement expression in Chrome, compared CSS-pixel `x`, `y`, `width`, and `height`, and emitted `engine-drift` lints above 2 px.

| Fixture | Nodes | Maximum box drift | Verdict |
|---|---:|---:|---|
| Flex launch card | 6 | 0.922 px | pass |
| Grid pricing | 12 | 1.781 px | pass |
| Type rhythm | 7 | 3.672 px | `engine-drift` |
| Container-query card | 9 | 1.563 px | pass |
| SVG + aspect ratio | 8 | 0.125 px | pass |

All 42 nodes existed in both engines. Same-origin parent patching succeeded in all five WKWebView and Chrome frames. Four fixtures stayed within the 2 px budget. The one lint is `type-rhythm / f3-mark / width`: WKWebView 83.766 px versus Chrome 87.438 px. It is system-font text (`-apple-system`, 44 px, weight 800, negative tracking), confirming the existing rule that production studios bundle fonts instead of treating system font metrics as canonical.

The five Chrome reference images were inspected: the flex, grid, typography, container-query, and transformed SVG fixtures rendered without clipping, missing content, or broken layout. The machine-readable result is `spikes/out/engine-drift/report.json`.

Conclusion: the M0 same-origin WKWebView patch criterion passes. The box-drift criterion takes its documented alternative: one divergent node exceeds 2 px and is emitted as an `engine-drift` lint. Native raster capture and perceptual comparison now pass all five fixtures: meaningful-pixel ratios are 0.060%–0.217% and block SSIM is 0.9727–0.9958. The largest residual is the known system-font difference in `type-rhythm`; Chrome remains the canonical export renderer.

#### Golden fidelity runbook

Chrome is the export authority; WKWebView is the editing preview. The shared five-fixture corpus covers flex, grid, typography, container queries, gradients, shadows, transforms, SVG, and overflow at a fixed 1080 × 1080 CSS viewport.

```sh
# Rewrite the committed Chrome references only after an intentional renderer change.
spikes/scripts/hypercanvas-fidelity.sh --update

# Capture fresh native WKWebView PNGs, compare them, and fail on threshold breach.
spikes/scripts/hypercanvas-fidelity.sh --compare
```

`--compare` writes normalized WebKit captures to `spikes/out/golden/webkit/`, red/yellow diff overlays with cyan region boxes to `spikes/out/golden/diff/`, WebKit box metrics to `spikes/out/golden/webkit-metrics.json`, and the machine-readable perceptual report to `spikes/out/golden/report.json`. Red pixels exceed the anti-aliasing neighborhood tolerance; yellow pixels differ directly but match within a one-pixel neighborhood. A fixture fails when meaningful pixels exceed 0.8% or block SSIM falls below 0.970.

Diagnosis order: inspect reported region boxes; check the matching `data-n` boxes in `webkit-metrics.json`; then classify the divergence as layout, bundled-font metrics, WebKit paint, or Chrome export behavior. Fix shared HTML/CSS for layout drift. Keep a WebKit-only preview normalization only when Chrome export remains correct and the rule is explicit. Never refresh Chrome goldens to bless a WebKit-only regression.
