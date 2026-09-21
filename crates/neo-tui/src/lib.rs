#![forbid(unsafe_code)]
//! Terminal front end (P12, plans/14-tui.md): renders state the core pushes and
//! sends typed commands. It holds no secrets and makes no decisions.
//!
//! The seam is [`neo_agent::runtime::Runtime`]; the split here mirrors 14 §4:
//! [`state`] is the pure reducer, [`keys`] is the pure binding table, [`ui`] is
//! the pure renderer, [`runs`] is the registry of what this front end started,
//! and [`run`] is the only module that owns the terminal, the clock and the
//! core handle.

pub mod keys;
pub mod run;
pub mod runs;
pub mod state;
pub mod ui;

pub use keys::{Action, KeyMap};
pub use run::{TuiError, run};
pub use runs::{Run, RunKind, RunState, TraceKind, TraceLine};
pub use state::{
    Activity, CARD_ARM_MS, Card, CardKind, Command, CommandSpec, FieldKind, Login, LoginPhase,
    Mode, NavSpec, Pane, Prompt, PromptKind, Row, RowAction, Section, SessionRow, Sessions, State,
    StepCard, ThreadRow, TurnProgress, View,
};
pub use ui::{Painted, draw};
