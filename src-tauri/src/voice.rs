//! One microphone session for the desktop, with explicit OpenAI transcription.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use neo_agent::{Runtime, RuntimeError};
use neo_voice::{Backend, MAX_UTTERANCE, Microphone, SPECTRUM_BINS, VoiceError};
use tauri::State;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::error::{CANCELLED, UiError};
use crate::state::Desktop;
use crate::view::Fix;

type TranscriptReply = oneshot::Sender<Result<String, UiError>>;

struct Active {
    cancel: CancellationToken,
    finished: CancellationToken,
    stop: Option<oneshot::Sender<TranscriptReply>>,
}

struct Session {
    status: &'static str,
    spectrum: [f32; SPECTRUM_BINS],
    error: Option<UiError>,
    active: Option<Active>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            status: "idle",
            spectrum: [0.0; SPECTRUM_BINS],
            error: None,
            active: None,
        }
    }
}

#[derive(Default)]
pub struct Voice {
    session: Arc<Mutex<Session>>,
}

fn lock(session: &Mutex<Session>) -> MutexGuard<'_, Session> {
    session.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Voice {
    /// Called on window teardown as well as when managed state is dropped.
    pub fn shutdown(&self) {
        if let Some(active) = &lock(&self.session).active {
            active.cancel.cancel();
        }
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn cancelled() -> UiError {
    UiError::new(CANCELLED, "dictation cancelled")
}

fn voice_error(error: VoiceError) -> UiError {
    let code = match &error {
        VoiceError::MissingOpenAiKey => "missing_key",
        VoiceError::NoInputDevice | VoiceError::UnknownDevice { .. } => "microphone_missing",
        VoiceError::MicrophoneDenied => "microphone_permission",
        VoiceError::TooLong { .. } => "voice_too_long",
        VoiceError::NoAudio => "voice_no_audio",
        _ => "voice",
    };
    let result = UiError::new(code, error.to_string());
    if matches!(error, VoiceError::MissingOpenAiKey) {
        result.with_fix(Fix::SetKey { account: "openai".into() })
    } else {
        result
    }
}

#[tauri::command]
pub async fn voice_start(desktop: State<'_, Desktop>, voice: State<'_, Voice>) -> Result<(), UiError> {
    let runtime = desktop.runtime();
    let session = Arc::clone(&voice.session);
    let cancel = CancellationToken::new();
    let finished = CancellationToken::new();
    let (stop_tx, stop_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    {
        let mut state = lock(&session);
        if state.active.is_some() {
            return Err(UiError::new("voice_busy", "dictation is already active; stop or cancel it first"));
        }
        state.status = "starting";
        state.error = None;
        state.spectrum.fill(0.0);
        state.active = Some(Active {
            cancel: cancel.clone(),
            finished: finished.clone(),
            stop: Some(stop_tx),
        });
    }
    // This task owns the session even if the invoking webview disappears.
    tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        let mut reply = None;
        let outcome = record(runtime, &session, &cancel, stop_rx, &mut ready, &mut reply).await;
        {
            let mut state = lock(&session);
            state.status = "idle";
            state.spectrum.fill(0.0);
            state.error = outcome.as_ref().err().filter(|error| error.code != CANCELLED).cloned();
            state.active = None;
            finished.cancel();
        }
        if let Some(ready) = ready {
            let _ = ready.send(outcome.as_ref().map(|_| ()).map_err(Clone::clone));
        }
        if let Some(reply) = reply {
            let _ = reply.send(outcome);
        }
    });
    ready_rx.await.map_err(|_| UiError::new("voice", "the dictation task stopped unexpectedly"))?
}

async fn record(
    runtime: Arc<Runtime>,
    session: &Arc<Mutex<Session>>,
    cancel: &CancellationToken,
    mut stop: oneshot::Receiver<TranscriptReply>,
    ready: &mut Option<oneshot::Sender<Result<(), UiError>>>,
    reply: &mut Option<TranscriptReply>,
) -> Result<String, UiError> {
    let start_cancel = cancel.clone();
    let (mut microphone, transcriber) = tokio::task::spawn_blocking(move || {
        let transcriber = runtime.transcriber_for(Backend::OpenAi).map_err(|error| match error {
            RuntimeError::Voice(error) => voice_error(error),
            other => UiError::from(other),
        })?;
        if start_cancel.is_cancelled() {
            return Err(cancelled());
        }
        let mut microphone = Microphone::open(None).map_err(voice_error)?;
        microphone.start().map_err(voice_error)?;
        Ok::<_, UiError>((microphone, transcriber))
    }).await??;
    if cancel.is_cancelled() {
        tokio::task::spawn_blocking(move || drop(microphone)).await?;
        return Err(cancelled());
    }
    // Enabling analysis is lazy, so the TUI's peak-only meter pays no FFT cost.
    let _ = microphone.spectrum();
    lock(session).status = "listening";
    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(()));
    }
    let deadline = tokio::time::sleep(MAX_UTTERANCE);
    tokio::pin!(deadline);
    let mut poll = tokio::time::interval(Duration::from_millis(40));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                tokio::task::spawn_blocking(move || drop(microphone)).await?;
                return Err(cancelled());
            }
            request = &mut stop => {
                *reply = Some(request.map_err(|_| cancelled())?);
                break;
            }
            () = &mut deadline => {
                tokio::task::spawn_blocking(move || drop(microphone)).await?;
                return Err(voice_error(VoiceError::TooLong { limit: MAX_UTTERANCE }));
            }
            _ = poll.tick() => {
                if microphone.capture_finished() {
                    // Retrieve the actual device/cap error rather than leaving a
                    // dead stream labelled Listening. Never auto-transcribe.
                    let result = tokio::task::spawn_blocking(move || microphone.stop()).await?;
                    return Err(match result {
                        Err(error) => voice_error(error),
                        Ok(_) => UiError::new("voice", "microphone capture ended unexpectedly; try recording again"),
                    });
                }
                lock(session).spectrum = microphone.spectrum();
            }
        }
    }
    let utterance = tokio::task::spawn_blocking(move || microphone.stop()).await?.map_err(voice_error)?;
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(cancelled()),
        transcript = transcriber.transcribe(&utterance) => {
            let text = transcript.map_err(voice_error)?.text;
            if text.trim().is_empty() {
                return Err(UiError::new("voice_no_speech", "No speech was recognized; check your microphone and try again."));
            }
            Ok(text)
        }
    }
}

/// The tuple uses ordinary bridge primitives: phase followed by FFT magnitudes.
#[tauri::command]
pub fn voice_status(voice: State<'_, Voice>) -> Result<(&'static str, Vec<f32>), UiError> {
    let state = lock(&voice.session);
    if let Some(error) = &state.error {
        return Err(error.clone());
    }
    Ok((state.status, state.spectrum.to_vec()))
}

#[tauri::command]
pub async fn voice_stop(voice: State<'_, Voice>) -> Result<String, UiError> {
    let (reply, receive) = oneshot::channel();
    {
        let mut state = lock(&voice.session);
        if state.status != "listening" {
            return Err(UiError::new("voice_busy", "dictation is not listening; wait for the current operation to finish"));
        }
        let sender = state.active.as_mut().and_then(|active| active.stop.take())
            .ok_or_else(|| UiError::new("voice_busy", "dictation is already stopping"))?;
        sender.send(reply).map_err(|_| UiError::new("voice", "the dictation task stopped unexpectedly"))?;
        state.status = "transcribing";
        state.spectrum.fill(0.0);
    }
    receive.await.map_err(|_| UiError::new("voice", "the dictation task stopped unexpectedly"))?
}

#[tauri::command]
pub async fn voice_cancel(voice: State<'_, Voice>) -> Result<(), UiError> {
    let finished = {
        let mut state = lock(&voice.session);
        state.error = None;
        state.active.as_ref().map(|active| {
            active.cancel.cancel();
            active.finished.clone()
        })
    };
    if let Some(finished) = finished {
        // Do not acknowledge cancellation until startup/stop has also released
        // its microphone. A following Start therefore cannot overlap it.
        finished.cancelled().await;
    }
    Ok(())
}
