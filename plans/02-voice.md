# 02 — Voice (`neo-voice`)

Crate `neo-voice`. Milestones **M2 — ears** (capture → VAD → STT → Conversation) and **M12 — voice out** (TTS, duplex, spoken questions, voice confirms). Pure Rust, no Tauri dependency; the whole pipeline runs headless under `neo voice …`.

Voice is the primary input (P4). **Always-on listening is the default, with open addressing** (P5): no wake word — every utterance heard while Listen is on is eligible, and Jev intake decides what it was. Push-to-talk and name-required are off-by-default settings. The bot can talk back, off by default, **questions only** when on (P6).

## Pipeline

```
cpal input (native rate, f32)            ← real-time callback: copy only
   └─ rtrb SPSC ring ─▶ dsp thread
        ├─ downmix → rubato Fft 48k→16k
        ├─ level meter (RMS/peak, 30 Hz) ───────────────▶ mic_levels channel → waveform
        ├─ earshot VAD (256-sample / 16 ms frames)
        ├─ duplex gate (M12: discards while the bot is speaking through speakers)
        └─ segmenter ─▶ Utterance { id, pcm16k, started_at, dur } ─▶ tokio
                           │ HearingStarted / HearingEnded (instant, local)
                           └─ Transcriber ─▶ Transcript
                                 └─ pre-intake filter ─▶ local control vocabulary ─▶ pending ask_user? ─▶ Jev intake (neo-judge)

Sol `say` / confirm sentence ─▶ Speaker (gpt-4o-mini-tts, pcm 24k stream) ─▶ rubato 24k→device ─▶ rtrb ─▶ cpal output
```

Crate layout: `capture.rs` · `resample.rs` · `vad.rs` (`trait Vad`, `EarshotVad`) · `segment.rs` · `stt/{mod,batch,live}.rs` · `filter.rs` (pre-intake + control vocabulary) · `listen.rs` (state machine) · `tts.rs` · `player.rs` · `duplex.rs` · `devices.rs` (CoreAudio transport/data-source queries) · `permission.rs` · `events.rs`.

## Capture — `cpal` 0.18

- 0.18 specifics: streams **start paused** on CoreAudio (`play()` explicitly), default 48 kHz, `F32` first, a single `cpal::Error` with `kind()`, `StreamConfig` by value, `Stream` is not `Clone` (and not `Send` — it lives on the thread that built it).
- The callback does **no alloc / lock / log** — it pushes into an `rtrb` ring (2 s capacity); overflow increments a counter surfaced in `Health.mic`.
- Device disconnect or default-device change → tear down, rebuild, recalibrate the noise floor; state is `MIC LOST` until a stream is running again (retry every 2 s and on every CoreAudio device-list change).
- Mic device is a setting; default = system default. **"Avoid Bluetooth microphones" is on by default**: opening a Bluetooth headset's mic drops the headset to the low-quality call profile, so when the default input is Bluetooth and a built-in mic exists, the built-in mic is used *(verify detection via the device transport type)*.
- **Listen off drops the stream** — not "ignores samples" — so macOS's orange mic indicator goes out. That is the honesty test for the toggle: the orange dot is on exactly when the UI says it is listening.

## Resample — `rubato` 5

`Fft` fixed-ratio resamplers: 48k→16k for VAD/STT, 48k→24k only when streaming STT is on, 24k→device rate for TTS. `process_into_buffer()` is allocation-free. The v5 API is new — `SincFixedIn`-era examples are stale.

## VAD — `earshot`

Pure Rust, ~40 KiB model, 16 kHz / 256-sample frames. Chosen over Silero-via-`ort` because ONNX Runtime means either a static-link dance or a dylib that must be signed inside the bundle for notarization.

```rust
pub trait Vad: Send { fn frame_len(&self) -> usize; fn is_speech(&mut self, frame: &[i16]) -> f32; fn reset(&mut self); }
```

`EarshotVad` is the only shipped impl; the trait keeps Silero / TEN swappable if the M0 spike shows earshot is not good enough in a noisy room. Tests use `ScriptedVad`.

## Segmenter

| Param | Default |
|---|---|
| pre-roll kept before speech start | 400 ms |
| start trigger | 4 consecutive speech frames (64 ms) above the noise gate |
| hangover (silence that closes an utterance) | **550 ms** (34 × 16 ms = 544 ms in the segmenter); **350 ms** while a task is running and the speech so far is ≤ 1.0 s (makes a spoken "stop" fast) |
| minimum utterance | 250 ms of speech — shorter is discarded locally, never uploaded |
| forced cut | 30 s |
| noise gate | calibrated floor + 6 dB |

State machine: `Idle → Speech → Hangover → (emit | back to Speech)`. `HearingStarted` fires on `Idle → Speech`, `HearingEnded` on emit/discard — the listening bar reacts before any network call.

**Noise-floor calibration**: 1 s of room noise when the stream opens (startup, unmute, device change, unlock); thereafter the floor tracks the 10th-percentile RMS of non-speech frames over a 30 s window, so a fan switching on does not open the gate forever. A frame counts as speech only if VAD says so **and** RMS is above the gate — this is also what keeps distant office chatter from being uploaded.

Addressing-mode variants: **push-to-talk** opens the stream only while the hotkey is held (orange dot only then) and the utterance is key-down → key-up, no VAD; **name-required** runs the normal pipeline and drops transcripts that do not contain the bot name (token match, edit distance ≤ 1, plus user aliases) as `not addressed`, greyed.

## Speech-to-text

```rust
pub trait Transcriber: Send + Sync { fn begin(&self, hints: &SttHints) -> Box<dyn SttSession>; }
#[async_trait] pub trait SttSession: Send {
    async fn push(&mut self, pcm16k: &[i16]);                          // live impl streams; batch impl buffers
    async fn finish(self: Box<Self>) -> BoxStream<'static, TranscriptEvent>;   // Partial(text) … Final(text) | Error
    async fn abort(self: Box<Self>);
}
pub struct SttHints { pub prompt: String, pub keywords: Vec<String>, pub language: Option<String> }
```

Both modes below implement it; implementations come from `SpeechProvider` (08).

### Hints (both modes)

- `prompt`: short and stable — `"Spoken requests to a desktop assistant named {name}."` (name from Identity, P8).
- `keywords` (≤ 100 *(verify the API's limit)*, rebuilt on app launch/quit and on settings change): **running app names** (regular-activation apps, supplied by the host through a `KeywordSource` trait — `neo-voice` does not depend on `neo-ax`), the **bot name + aliases**, the **user vocabulary** (Settings → Voice), and **routine names** (so `routine:<name>` routes are heard right). **Window titles are never sent** — they can carry private text.

### Default: per-utterance `gpt-transcribe`

`POST /v1/audio/transcriptions`, multipart, 16 kHz mono WAV written with `hound` (≤ 30 s ≈ 1 MB, far under the 25 MB cap).

- `stream=true` optional (`transcript.text.delta` / `.done`) — used only for utterances over 8 s.
- Billed per minute **of uploaded speech only** — silence never leaves the machine.
- Raw `reqwest` through the provider's `{ base_url, credential }`; the model and its `keywords` param are weeks old and typed clients lag.
- Timeout 8 s, one retry on 429/5xx; failure → the utterance appears in the Conversation as `couldn't transcribe` with a retry affordance (audio is held in memory for 60 s for that purpose only).

**Hidden, never offered** (K3): `whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`, `gpt-4o-transcribe-diarize` — deprecated 2026-08-26, shutdown 2027-02-26.

### Option: streaming `gpt-live-transcribe`

`wss://api.openai.com/v1/realtime?intent=transcription` (`tokio-tungstenite`), first message:

```json
{"type":"session.update","session":{"type":"transcription","audio":{"input":{
  "format":{"type":"audio/pcm","rate":24000},
  "transcription":{"model":"gpt-live-transcribe","keywords":[…],"delay":"low"},
  "turn_detection":null}}}}
```

**Local VAD stays the authority** (`turn_detection: null`): `input_audio_buffer.append` (base64 pcm16 24 kHz, pre-roll included) while the segmenter is in `Speech`/`Hangover`, `input_audio_buffer.commit` on emit, `…clear` on discard. Read `conversation.item.input_audio_transcription.delta` / `.completed`. The socket is opened on Listen-on, kept alive with pings, closed on MUTED/PAUSED, reconnected with backoff; while it is down the batch transcriber takes over silently. Buys live partials in the Conversation at roughly 4× the price. *(verify: the `noise_reduction` field; whether the `OpenAI-Beta` header is still needed; session max lifetime)*

## Pre-intake filter and local control vocabulary

Runs on the final transcript, **before any further network call** (no Jev request is spent). Order matters:

**1. Normalise** — lowercase, strip punctuation, strip a leading/trailing bot name ("stark, stop" → "stop").

**2. Local control vocabulary** — the **whole** normalised utterance must equal a form ("stop the video at 0:30" is a task, not a stop):

| Forms | Effect | When |
|---|---|---|
| `stop · cancel · abort · never mind · nevermind · stop it · cancel that` | kill the running task: step-boundary stop, `player.stop()`, release modifiers, clear any pending confirm | a task is running or waiting; otherwise falls through to intake |
| `yes · yeah · yep · approve · go ahead · do it · confirm` | approve the pending confirm | **only** when a voice-eligible confirm is pending |
| `no · nope · deny · don't` | deny the pending confirm | whenever any confirm is pending (denying is always safe) |
| `mute · stop listening` | Listen off (stream dropped). Unmuting is by hotkey, tray, pill or bar — never by voice, the mic is off. | always |

The zero-network stop is the kill hotkey; a spoken "stop" still costs hangover + STT (~1 s with the short hangover).

**3. Drops** — not sent to intake, but **still shown greyed in the Conversation** with the reason, and force-enqueueable by click:

- fewer than 2 words and not a control form;
- known STT silence hallucinations: "Thank you.", "Thanks for watching.", "you", "Bye.", "Subtitles by…" (list is data, extendable);
- **self-echo**: token-set similarity ≥ 0.8 with any line the bot spoke in the last 10 s (belt-and-braces behind the duplex gate);
- name-required mode and no name present;
- looks like a secret (`sk-…`, long hex/base64 runs): the text is **redacted before storage** and dropped — keys are never accepted by voice (K1).

**4. Pending `ask_user`** → the transcript is the answer (below). **5. Otherwise** → Jev intake in `neo-judge` (A7), with `source = voice`.

## Listen state

One `ListenState`, owned here, mirrored by the listening bar, tray, pill and quick entry. Displayed state = the highest-priority condition that holds:

| Priority | State | Condition | Stream |
|---|---|---|---|
| 1 | `NO PERMISSION` | TCC denied / undetermined | closed |
| 2 | `MIC LOST` | no input device, or stream build failing | closed, retrying |
| 3 | `PAUSED` | **screen locked or display asleep (P7)** | **closed** |
| 4 | `MUTED` | Listen off | closed |
| 5 | `SPEAKING` | TTS playing | open (gated or live, per duplex mode) |
| 6 | `HEARING YOU` | segmenter in `Speech`/`Hangover` | open |
| 7 | `TRANSCRIBING` | ≥ 1 utterance awaiting its transcript | open — capture never stops for STT |
| 8 | `LISTENING` | otherwise | open |

**PAUSED (P7)**: on the platform layer's `ScreenLocked` / `DisplaySlept` event (05) → drop the stream (orange dot out — nothing is heard on a locked Mac), discard any open utterance without uploading it, abort in-flight STT, `player.stop()`. On unlock → if Listen was on, rebuild the stream and recalibrate; an unanswered `ask_user` is re-shown, not re-spoken. First launch starts Listen **on** after the onboarding statement; afterwards the last state is restored.

## Events and channels

`neo-voice` emits plain Rust (`tokio::sync::watch` for levels, `broadcast` for events); `src-tauri` bridges them (04), `neo voice listen` prints them.

| Name | Kind | Payload |
|---|---|---|
| `mic_levels` | channel, **30 Hz** | `[rms, peak]` |
| `tts_levels` | channel, 30 Hz | `[rms, peak]` — drives the waveform in `SPEAKING` |
| `partial_transcripts` | channel | `{ utterance_id, text }` (streaming STT only) |
| `ListenState` | event | `{ state, reason? }` |
| `HearingStarted` / `HearingEnded` | event | `{ utterance_id }` / `{ utterance_id, dur_ms, discarded: bool }` |
| `Transcript` | event | `{ utterance_id, text, latency_ms, disposition: intake \| control(kind) \| answer \| dropped(reason) }` → becomes a Conversation message |
| `Spoke` | event | `{ text, truncated, task_id? }` |
| `Health.mic` | event field | device name, ring overflows, STT error streak |

## Text-to-speech (M12; optional, off by default)

`POST /v1/audio/speech`, **`gpt-4o-mini-tts`**, voice **`marin`** (`cedar` alt), `response_format: "pcm"` = 24 kHz s16le mono headerless, chunked. `tts-1*` ids are hidden (K3).

- **Player**: bytes → carry an odd trailing byte across chunks → i16→f32 → rubato 24k→device rate → `rtrb` → `cpal` output. 80 ms prebuffer; underrun = silence, never a glitch loop. **First audio < 500 ms** target. `stop()` flushes the ring and aborts the HTTP body (kill switch, cancel, barge-in, lock). Output follows the system default device and is rebuilt on change.
- **`instructions`** are derived from `soul.md` → the *How I speak* section (P8): heading match, Markdown stripped, ≤ 600 chars, cached by content hash, refreshed when the file changes. Missing/empty → `"Brief, calm, matter-of-fact. No filler."` The section only ever fills the style field — it cannot change *what* is said or any rule (preferences, not permissions).
- **What is spoken** (setting): **`questions only`** (default when TTS is on — P6) · `questions + results` · `everything incl. step narration`. "Questions" = `ask_user` and confirm requests. Text comes only from Sol's `say` field (`ask_user` / `finish` / `fail`) or the confirm card's generated sentence — **never** from logs, page text or on-screen text. Navigator-only tasks (zero Sol calls) speak only their confirms.
- **Truncation**: 2 sentences or 240 chars, whichever is shorter; the full text is always in the Conversation bubble, whose first sentence is the spoken line.
- One line at a time; a new line replaces a queued one, never stacks.

## Duplex — not hearing itself

| Mode | When | Behaviour |
|---|---|---|
| **Half-duplex gate** (default) | output is speakers | while TTS plays **+ 200 ms tail**, frames are discarded before the segmenter. No voice barge-in; the kill hotkey, pill and UI stop button interrupt speech. |
| **Full duplex + barge-in** | output device is **headphones** | mic stays live; 200 ms of sustained speech → `player.stop()`, and the utterance proceeds normally. |
| **AEC** (later, opt-in) | speakers + barge-in wanted | `sys-voice` (Apple VoiceProcessingIO; system output is the echo reference automatically) — caveats: **ducks other audio, ~2 s init stall, weak Bluetooth volume**. Alternative: `webrtc-audio-processing` 2.x with TTS frames as the render reference (a C++ build in the bundle). Neither is in M12. |

**Headphones detection** (`devices.rs`, CoreAudio properties cpal does not expose): built-in output with data source `hdpn` → headphones; transport Bluetooth / BluetoothLE → assumed headphones *(verify: Bluetooth speakers are misdetected)*; USB, HDMI, DisplayPort, AirPlay, built-in speaker → speakers. The user can override per device, remembered by device UID. Re-evaluated on every output-device change, mid-sentence included.

## Answers, confirms, secrets

- **After `ask_user`** the next utterance **bypasses Jev intake** and goes straight to the waiting task as the answer. The window opens when the question finishes playing (or when the card appears, TTS off) and lasts **30 s**; after that the question stays in the UI only and speech goes through intake again (where the `answer` intent can still catch it). Stop forms win over the bypass.
- **Voice confirms** accept **only the exact yes/no forms** above — anything else ("yeah send it to Dana too") goes to intake as an amend and the confirm stays pending. A "yes" is ignored unless it *started* ≥ 600 ms after the card appeared / the spoken question ended. Each confirm carries `voice_ok`; defaults: `outward` on, **`destructive` and `spends` click-only**, and Settings → Safety can make every confirm click-only. A click-only confirm is spoken as "I need you to approve this on screen."
- **Keys and secrets are never accepted by voice** (K1): an `ask_user` of kind `secret` disables the bypass and the UI shows a paste field owned by `neo-keys`; the bot never reads a secret aloud.

## Permission

- `NSMicrophoneUsageDescription` in `src-tauri/Info.plist` — without it macOS kills the process with no prompt. String: *"starkbot-neo listens for your spoken requests while Listen is on. Only detected speech is sent for transcription."*
- Hardened runtime entitlement `com.apple.security.device.audio-input`.
- Check/request via **`objc2-av-foundation`** (`AVCaptureDevice authorizationStatusForMediaType:` / `requestAccessForMediaType:`) — small enough to own rather than depend on `tauri-plugin-macos-permissions` (v2.3.0, ~1 yr stale; read for reference). Denied → `NO PERMISSION` with a deep link to Privacy › Microphone; macOS will not re-prompt.
- Under `tauri dev` the prompt is **attributed to the launching terminal**, and IDE-integrated terminals can fail silently — run dev from Terminal.app/iTerm. `neo doctor` reports status and attribution.

## Privacy stance (shown in onboarding, enforced in code)

- **Audio is never written to disk** by default. "Keep recordings for debugging" (off) keeps a ring of the last 20 utterances in the app-data dir, deletable.
- **Only VAD-positive, gate-passing segments are uploaded**, and only to the configured `SpeechProvider` (OpenAI today). Locked or muted = the stream is closed, not ignored.
- Transcripts — including dropped ones — are stored **locally** (SQLite) and deletable individually or all at once in Settings → Privacy.
- STT hints never include window titles, document names or page text.

## Cost (indicative, 2026-09-18 — the app reads prices from the live price table, K3)

| Item | Price | Typical day |
|---|---|---|
| `gpt-transcribe` | $0.0045 / min of uploaded speech | 40 min addressed + overheard speech ≈ **$0.18** |
| `gpt-live-transcribe` | ≈ 4× *(verify)* | same day ≈ $0.72 |
| `gpt-4o-mini-tts`, questions only | ≈ $0.015 / min of audio *(verify)* | 20 questions × 5 s ≈ $0.03 |
| Jev intake per surviving utterance | token-metered (shown as tokens until priced) | — |

Open addressing means overheard speech is paid for: a talkative office at 3 h/day ≈ $0.81. The Voice settings show **speech minutes today**, and STT spend counts toward the daily cap (K5); hitting the cap switches to `MUTED` with a banner.

## Latency budget — end of speech → transcript **< 1.5 s**

| Part | Time |
|---|---|
| hangover | 550 ms (350 ms short form) |
| WAV encode + request build | 2–8 ms measured in S4 |
| upload + `gpt-transcribe` (5.3 s utterance) | 810 ms p50 / 1,299 ms p95 over five S4 calls |
| filter + control match | < 1 ms target |
| **total** | **1,362 ms p50 / 1,845 ms p95 measured**; spoken "stop" projects ≈ 1.2 s at the same request p50 |
| then Jev intake | + ~100–180 ms |
| TTS first audio | < 500 ms from `say` |

The S4 spike applied the first lever: 700 ms missed p50, while the 550 ms normal hangover passed both budgets. Speculative upload at 350 ms stays out of v1 unless broader fixture measurements regress; it adds cancellation and billing ambiguity without evidence that it is needed.

## Provider seam

`SpeechProvider` (08) hands out `Box<dyn Transcriber>` and `Box<dyn Speaker>` for a `ModelRef`; `OpenAiDirect` now, `StarkRouter` later (K2). No vendor URL or credential exists in `neo-voice` — clients take `{ base_url, credential }` from the provider, and secrets stay in `neo-keys`.

```rust
#[async_trait] pub trait Speaker: Send + Sync {
    async fn speak(&self, text: &str, style: &str) -> Result<BoxStream<'static, Result<Bytes>>>;   // pcm s16le 24 kHz mono
}
```

## Test plan (Rust only)

| Area | Test |
|---|---|
| Segmenter | **WAV fixtures** (`fixtures/*.wav` + `*.expected.json`): quiet room, music playing, two speakers, keyboard noise, long monologue (forced cut), a lone "stop" during a task (short hangover). Boundaries within ± 50 ms, with `ScriptedVad` and with real earshot. |
| Noise floor | fan-on fixture: gate re-opens within 30 s; no endless utterance |
| Resampler / player | odd-byte carry across chunk splits at every offset; 24k→44.1k/48k length and no clicks at chunk joins; `stop()` silences within one buffer |
| STT clients | `wiremock`: multipart shape, `prompt` + `keywords` present, deprecated ids rejected, retry/timeout; local WS server: session.update shape, append/commit/clear ordering, reconnect → batch fallback |
| Filter + control | table tests: every form, whole-utterance rule, bot-name stripping, yes ignored with no confirm, yes ignored inside 600 ms, click-only confirm ignores yes, hallucination list, secret redaction |
| **Echo** | TTS PCM mixed into a mic fixture at −6 dB: half-duplex gate emits **zero** utterances incl. the 200 ms tail; gate off → self-echo filter drops it; full-duplex fixture → barge-in stops the player within 250 ms |
| Listen state | priority table; lock mid-utterance → nothing uploaded, stream closed; unlock restores; device loss → `MIC LOST` → recovery |
| Bypass | answer window opens after TTS end, closes at 30 s, stop wins, `secret` asks never bypass |
| Live (ignored by default, needs a key) | `neo voice bench` on recorded fixtures: latency p50/p95, word accuracy on app names with vs without `keywords` |

Fixtures are recorded with `neo voice record` — an explicit CLI act, the only way audio reaches disk outside the debug toggle.

## CLI surface (`neo voice …`)

`devices` (inputs/outputs, transport, headphones guess) · `meter` · `listen [--live] [--no-stt]` (utterance boundaries, transcripts, dispositions, latency) · `transcribe file.wav` · `segment file.wav` (boundaries only, offline) · `record out.wav` · `say "text" [--voice marin]` · `duplex` (speaks while listening, reports leaked utterances) · `bench` · `doctor` (permission, attribution, device, provider reachability).

## Acceptance criteria

**M2 — ears**
1. Listen on → orange dot on within 1 s; Listen off, screen lock and display sleep → orange dot **off**; state identical on bar, tray, pill.
2. `HearingStarted` reaches the UI < 150 ms after speech onset; `mic_levels` at 30 Hz with no React re-render per sample.
3. End of speech → transcript **p50 < 1.5 s**, p95 < 2.5 s on a 3–5 s utterance; spoken "stop" halts a running task p50 < 1.2 s.
4. Segmenter fixtures pass; 10 min of music alone produces ≤ 2 uploaded utterances; 10 min of silence produces 0.
5. Running-app names and the bot name transcribe correctly in ≥ 9/10 trials with `keywords`.
6. Dropped utterances appear greyed with a reason and can be force-enqueued; typed messages share the thread.
7. Unplugging the mic → `MIC LOST` → automatic recovery on re-plug; zero ring overflows in a 1 h soak.
8. No audio file exists on disk after a session with the debug toggle off (test asserts on the app-data dir).

**M12 — voice out**
1. TTS off by default; when on, only `ask_user` and confirm lines are spoken by default.
2. First audio < 500 ms p50; `stop()` (kill switch, cancel, lock) silences < 100 ms.
3. Speakers: a 10-question session yields **zero** self-heard utterances. Headphones: auto-detected, barge-in stops speech < 250 ms.
4. Answer bypass works inside 30 s and never for `secret` asks; exact-form yes/no resolves voice-eligible confirms; `destructive` / `spends` confirms cannot be approved by voice.
5. Editing *How I speak* in `soul.md` changes the delivery of the next spoken line and nothing else.

## Risks

| Risk | Mitigation |
|---|---|
| Open addressing uploads overheard speech (cost + privacy) | RMS gate + calibration, speech-minutes meter, daily cap → MUTED, push-to-talk / name-required one click away |
| earshot weak in noise or with music | `trait Vad` seam; M0 spike decides; music fixture in CI |
| STT hallucinations on near-silence | min-speech rule, gate, hallucination list, all drops visible |
| New OpenAI params (`keywords`, transcription session shape) shift | raw `reqwest`/WS, shapes pinned by mock tests, *(verify)* items closed in the M0 spike |
| 550 ms hangover cuts slow talkers | tunable; interrupted speech returns from `Hangover` to `Speech`; fixture boundaries and slow-speech recordings gate release |
| Bluetooth mic degrades headphone audio | avoid-Bluetooth-mic default |
| Headphones misdetected → bot hears itself | self-echo filter behind the gate; per-device override |
| VoiceProcessingIO side effects | AEC stays opt-in and out of M12 |
| A stale "yes" approves the wrong card | exact forms, 600 ms rule, click-only for `destructive`/`spends` |
| `tauri dev` mic attribution confusion | `neo doctor` names the attributed process |
