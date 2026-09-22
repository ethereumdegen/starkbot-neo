//! Key event -> `Action` (14 §3). Pure: it reads `State` and returns an intent,
//! it never mutates anything and it never talks to the core.
//!
//! Three rules here are not negotiable:
//!
//! * quitting is explicit — `Ctrl-Q`, or `q` at top level which only *asks*;
//! * no key resolves a confirm unless the card's action sentence is on screen
//!   and has been armed for the debounce window (04 §13, 14 §3);
//! * `Esc` interrupts. While anything this front end started is live, one
//!   press of `Esc` stops it — an agent a user cannot stop with the key they
//!   already reach for is not interruptible in any way that counts.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::{Mode, Pane, State, View};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    AskQuit,
    ConfirmQuit,
    CancelQuit,
    Redraw,
    ToggleHelp,
    CloseOverlay,
    KillSwitch,
    /// `x`: cancel the run the Mind pane is tracing.
    StopRun,
    ToggleFollow,
    EnterInsert,
    LeaveInsert,
    EnterCommand,
    ComposerChar(char),
    ComposerNewline,
    ComposerBackspace,
    ComposerDeleteWord,
    ComposerDeleteLine,
    ComposerSubmit,
    LineChar(char),
    LineBackspace,
    LineComplete,
    LineSubmit,
    LineCancel,
    OpenProject,
    ProjectBack,
    SelectNext,
    SelectPrevious,
    SelectFirst,
    SelectLast,
    SelectPageDown,
    SelectPageUp,
    SelectHalfDown,
    SelectHalfUp,
    Activate,
    BeginSetKey,
    RemoveKey,
    ConnectSubscription,
    DisconnectSubscription,
    /// Inside the login overlay.
    LoginOpenPage,
    LoginBeginPaste,
    LoginPasteChar(char),
    LoginPasteBackspace,
    LoginSubmitPaste,
    LoginClose,
    RefreshModels,
    CheckKey,
    /// Start or stop dictation (push-to-talk, on a toggle because a terminal
    /// cannot see a held key).
    ToggleDictation,
    /// `m`: flip `listen.enabled`, which is what the desktop app's mute does.
    ToggleListen,
    /// `Ctrl-N`.
    NewConversation,
    /// Inside the session picker.
    OpenSessions,
    SessionNext,
    SessionPrevious,
    SessionOpen,
    SessionClose,
    PromptChar(char),
    PromptBackspace,
    PromptSubmit,
    PromptCancel,
    /// Clear the whole buffer. A prompt that pre-fills — a model id, a
    /// settings field — is otherwise only editable one backspace at a time.
    PromptDeleteLine,
    ResolveConfirm {
        approve: bool,
    },
    ToggleRemember,
    ShowMe,
    AnswerAsk(u8),
    UnfocusCard,
    PendingG,
    Unavailable(&'static str),
}

/// The binding table. A struct rather than a free function because Settings →
/// Shortcuts will make it data; M1 ships the defaults only.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyMap;

impl KeyMap {
    #[must_use]
    pub fn resolve(&self, key: KeyEvent, state: &State) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);

        // Always reachable, in every mode, including inside a modal (14 §3).
        if control {
            match key.code {
                KeyCode::Char('c') => return Action::KillSwitch,
                KeyCode::Char('q') => return Action::Quit,
                KeyCode::Char('l') => return Action::Redraw,
                _ => {}
            }
        }

        if state.prompt.is_some() {
            return prompt_key(key, control);
        }
        // The login overlay owns the keyboard while a sign-in is in flight:
        // typing a redirect URL must not also scroll a pane behind it.
        if state.login.is_some() {
            return login_key(key, control, state);
        }
        if state.quit_prompt {
            return match key.code {
                KeyCode::Char('y') => Action::ConfirmQuit,
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => Action::CancelQuit,
                _ => Action::None,
            };
        }
        if state.help {
            return match key.code {
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    Action::ToggleHelp
                }
                _ => Action::None,
            };
        }
        if state.sessions.is_some() {
            return session_key(key, control);
        }
        if state.card.is_some() {
            return card_key(key, state);
        }
        match state.mode {
            Mode::Command => line_key(key, control),
            Mode::Insert => insert_key(key, control, state),
            // `Card` without a card is not reachable through any binding; treat
            // it as normal so a stray state can still be driven.
            Mode::Normal | Mode::Card => match state.view {
                View::Settings => settings_key(key, control, state),
                View::Panes => normal_key(key, control, state),
            },
        }
    }
}

/// Keys inside the conversation switcher.
fn session_key(key: KeyEvent, control: bool) -> Action {
    if let Some(action) = list_key(key, control) {
        // The picker is the whole list: every motion moves its cursor, never
        // a pane behind it.
        return match action {
            Action::SelectPrevious | Action::SelectHalfUp | Action::SelectPageUp => {
                Action::SessionPrevious
            }
            _ => Action::SessionNext,
        };
    }
    match key.code {
        KeyCode::Enter => Action::SessionOpen,
        KeyCode::Esc | KeyCode::Char('q') => Action::SessionClose,
        _ => Action::None,
    }
}

/// Keys inside the subscription login overlay (K7).
fn login_key(key: KeyEvent, control: bool, state: &State) -> Action {
    let pasting = state
        .login
        .as_ref()
        .is_some_and(|login| login.phase == crate::state::LoginPhase::Pasting);
    if pasting {
        return match key.code {
            KeyCode::Enter => Action::LoginSubmitPaste,
            KeyCode::Esc => Action::LoginClose,
            KeyCode::Backspace => Action::LoginPasteBackspace,
            // A redirect URL is long; a paste arrives as a burst of chars.
            KeyCode::Char(character) if !control => Action::LoginPasteChar(character),
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => Action::LoginClose,
        KeyCode::Enter | KeyCode::Char('o') => Action::LoginOpenPage,
        KeyCode::Char('p') => Action::LoginBeginPaste,
        _ => Action::None,
    }
}

fn prompt_key(key: KeyEvent, control: bool) -> Action {
    match key.code {
        KeyCode::Enter => Action::PromptSubmit,
        KeyCode::Esc => Action::PromptCancel,
        KeyCode::Backspace => Action::PromptBackspace,
        KeyCode::Char('u') if control => Action::PromptDeleteLine,
        KeyCode::Char(character) if !control => Action::PromptChar(character),
        _ => Action::None,
    }
}

/// `y`/`n` are live only while a card is focused, rendered and armed. `Enter`
/// never resolves anything.
fn card_key(key: KeyEvent, state: &State) -> Action {
    let armed = state
        .card
        .as_ref()
        .is_some_and(|card| card.armed && card.rendered);
    match key.code {
        KeyCode::Char('y') if armed => Action::ResolveConfirm { approve: true },
        KeyCode::Char('n') if armed => Action::ResolveConfirm { approve: false },
        KeyCode::Char('y' | 'n') => Action::None,
        KeyCode::Char('r') => Action::ToggleRemember,
        KeyCode::Char('s') => Action::ShowMe,
        KeyCode::Char('j') | KeyCode::Down => Action::SelectNext,
        KeyCode::Char('k') | KeyCode::Up => Action::SelectPrevious,
        KeyCode::Char(digit @ '1'..='9') => {
            Action::AnswerAsk(digit.to_digit(10).unwrap_or(0).try_into().unwrap_or(0))
        }
        KeyCode::Char('i') => Action::EnterInsert,
        KeyCode::Esc => Action::UnfocusCard,
        _ => Action::None,
    }
}

fn line_key(key: KeyEvent, control: bool) -> Action {
    match key.code {
        KeyCode::Enter => Action::LineSubmit,
        KeyCode::Esc => Action::LineCancel,
        KeyCode::Tab => Action::LineComplete,
        KeyCode::Backspace => Action::LineBackspace,
        KeyCode::Char(character) if !control => Action::LineChar(character),
        _ => Action::None,
    }
}

fn insert_key(key: KeyEvent, control: bool, state: &State) -> Action {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => Action::LeaveInsert,
        KeyCode::Enter if alt => Action::ComposerNewline,
        KeyCode::Char('j') if control => Action::ComposerNewline,
        KeyCode::Enter => Action::ComposerSubmit,
        KeyCode::Backspace => Action::ComposerBackspace,
        KeyCode::Char('/') if state.composer.is_empty() => Action::EnterCommand,
        KeyCode::Char('v') if control => Action::ToggleDictation,
        KeyCode::Char('n') if control => Action::NewConversation,
        KeyCode::Char('w') if control => Action::ComposerDeleteWord,
        KeyCode::Char('u') if control => Action::ComposerDeleteLine,
        KeyCode::Char('a') if control => {
            Action::Unavailable("attachments need the media import, which is not built yet")
        }
        // The transcript is readable without leaving the composer: typing a
        // long message and wanting to check what was said ten lines up is the
        // same moment, and `Esc k k k i` to do it is four keys too many.
        // `Up`/`Down` are not borrowed for it — they recall what was typed.
        KeyCode::PageUp => Action::SelectPageUp,
        KeyCode::PageDown => Action::SelectPageDown,
        KeyCode::Up if state.composer.is_empty() => {
            Action::Unavailable("nothing typed yet to recall")
        }
        KeyCode::Char(character) if !control => Action::ComposerChar(character),
        _ => Action::None,
    }
}

fn settings_key(key: KeyEvent, control: bool, state: &State) -> Action {
    if let Some(action) = list_key(key, control) {
        return action;
    }
    match key.code {
        KeyCode::Enter | KeyCode::Char(' ') => Action::Activate,
        KeyCode::Char('s') => Action::BeginSetKey,
        KeyCode::Char('x') => Action::RemoveKey,
        KeyCode::Char('c') => Action::ConnectSubscription,
        KeyCode::Char('d') => Action::DisconnectSubscription,
        KeyCode::Char('r') => Action::RefreshModels,
        // Shifted, because lowercase `k` is list motion everywhere else and
        // `list_key` above already claimed it. A check-key on `k` meant the
        // cursor could not be moved up at all.
        KeyCode::Char('K') => Action::CheckKey,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('g') if state.pending_g => Action::SelectFirst,
        KeyCode::Char('g') => Action::PendingG,
        KeyCode::Char('G') => Action::SelectLast,
        KeyCode::Esc | KeyCode::Char('q') => Action::CloseOverlay,
        _ => Action::None,
    }
}

fn normal_key(key: KeyEvent, control: bool, state: &State) -> Action {
    if control && key.code == KeyCode::Char('n') {
        return Action::NewConversation;
    }
    if state.focus != Pane::Conversation && matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        return Action::CloseOverlay;
    }
    if state.focus == Pane::Projects {
        match key.code {
            KeyCode::Enter => return Action::OpenProject,
            KeyCode::Backspace | KeyCode::Left if state.project_detail.is_some() => {
                return Action::ProjectBack;
            }
            _ => {}
        }
    }
    if let Some(action) = list_key(key, control) {
        return action;
    }
    match key.code {
        KeyCode::Char('i' | 'a') => Action::EnterInsert,
        KeyCode::Char('/') => Action::EnterCommand,
        KeyCode::Char('f') => Action::ToggleFollow,
        KeyCode::Char('v') => Action::ToggleDictation,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('x') => Action::StopRun,
        KeyCode::Char('m') => Action::ToggleListen,
        KeyCode::Char('g') if state.pending_g => Action::SelectFirst,
        KeyCode::Char('g') => Action::PendingG,
        KeyCode::Char('G') => Action::SelectLast,
        KeyCode::Esc if state.has_live_run() => Action::StopRun,
        KeyCode::Char('q') | KeyCode::Esc => Action::CloseOverlay,
        _ => Action::None,
    }
}

/// Motion shared by every list (14 §3, "Lists").
fn list_key(key: KeyEvent, control: bool) -> Option<Action> {
    let action = match key.code {
        KeyCode::Char('d') if control => Action::SelectHalfDown,
        KeyCode::Char('u') if control => Action::SelectHalfUp,
        KeyCode::Char('f') if control => Action::SelectPageDown,
        KeyCode::Char('b') if control => Action::SelectPageUp,
        KeyCode::PageDown => Action::SelectPageDown,
        KeyCode::PageUp => Action::SelectPageUp,
        KeyCode::Char('j') | KeyCode::Down => Action::SelectNext,
        KeyCode::Char('k') | KeyCode::Up => Action::SelectPrevious,
        _ => return None,
    };
    Some(action)
}
