# M0 spikes

Each of these answered one question that had to be settled before the thing it
informs could be designed. The questions are listed in [`PLAN.md`](../PLAN.md)
§3; the numbers and conclusions are in [`plans/spikes.md`](../plans/spikes.md),
which is the document to read. The code is the evidence behind it.

| Spike | Question |
|---|---|
| `s1-nav` | Rust → CDP: managed Chrome profile, snapshot script, one multi-head TypeSafe request, click the chosen control. Latency of each part. |
| `s4-voice` | earshot → `gpt-transcribe`: end-of-speech → transcript time; false-trigger rate in a normal room. |
| `s5-sol` | metalcraft + Sol + one tool with reasoning-item replay; do reasoning summaries come through `rig`? |
| `s6-panel` | Non-activating `NSPanel` above a fullscreen app; click-through ring. |
| `s7-render` | Canvas fidelity: HTML/CSS → CDP screenshot at 1×/2×/3× vs the same frame in the webview; deterministic frame-stepping of a CSS animation → ffmpeg. |
| `s7-webview` | The webview half of the same comparison. |

## They are not in the product workspace

The root manifest carries `exclude = ["spikes"]` and this directory is its own
workspace. Two reasons, both discovered when the lint policy was audited:

- CI compiled two extra Tauri apps on every push and nothing depended on them.
- None of them opted into `[lints] workspace = true`, so `unsafe_code = "deny"`
  — which `plans/05-platform.md` §1 rule 6 states holds workspace-wide, with
  `unsafe` confined to `neo-ax`, `neo-voice::perm` and the `src-tauri` panel
  glue — was never applied here. `s6-panel` and `s7-webview` contain raw
  `unsafe` blocks. The rule is now true of the workspace it claims to describe,
  and this code sits outside it.

They still build on macOS:

```sh
cd spikes && cargo build --workspace
```

They are not maintained. A spike that stops compiling against a changed
`neo-cdp` or `jev-nav` has done its job already; fix it only if you are
re-running the measurement.
