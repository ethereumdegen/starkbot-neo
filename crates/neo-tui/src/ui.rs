//! Rendering (14 §2). Pure: it reads `State` and writes cells, and reports
//! back one fact the reducer cannot know — whether the card's sentence
//! actually reached the frame (04 §13).
//!
//! Every state carries a word as well as a colour and a glyph, so `NO_COLOR`, a
//! two-colour terminal and a colour-blind reader all still read it (14 §2).

use neo_core::KeyState;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::runs::{Run, RunState, TraceKind};
use crate::state::{
    COMMAND_LINE, Card, CardKind, LoginPhase, Mode, Pane, Role, Row, Section, State, StepCard,
    TurnProgress, View, connection_label, key_label, listen_label,
};

/// Below this the TUI renders one line and keeps consuming events (14 §2).
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 20;

const GREY: Color = Color::DarkGray;

/// The card overlay's width. Wide enough for a sentence plus a URL on one
/// wrap, narrow enough to leave the frame around it readable.
const CARD_WIDTH: u16 = 72;
/// Below this a card is not drawn at all: there is no honest way to put a
/// sentence, its context and two keys in less.
const CARD_MIN_WIDTH: u16 = 40;

/// What the frame that was just drawn contains, for the decisions the
/// reducer makes about it afterwards.
///
/// One field today, and it is the important one: `y` may not resolve a
/// confirm whose sentence has not been on screen (04 §13, 14 §3), and only
/// the renderer knows whether it got there. The default is "nothing was
/// painted", so a frame that was never drawn can never arm a card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Painted {
    /// The card's sentence reached the frame and nothing covers it.
    pub card: bool,
}

pub fn draw(frame: &mut Frame, state: &State) -> Painted {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let line = Paragraph::new("terminal too small — 60×20 minimum")
            .alignment(Alignment::Center)
            .style(Style::new().fg(Color::Yellow));
        let [middle] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .areas(area);
        frame.render_widget(line, middle);
        return Painted::default();
    }

    let header_height = u16::from(area.height >= 24);
    let composer_height = u16::from(state.view == View::Panes && state.focus == Pane::Conversation);
    let [header, body, composer, status] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(3),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(area);

    if header_height > 0 {
        frame.render_widget(header_line(state), header);
    }
    match state.view {
        View::Panes => render_panes(frame, body, state),
        View::Settings => render_settings(frame, body, state),
    }
    if composer_height > 0 {
        frame.render_widget(composer_line(state), composer);
    }
    frame.render_widget(status_line(state, header_height == 0, status.width), status);

    // The card goes under the overlays that own the keyboard, because one of
    // them is how a free-text question gets answered and the question has to
    // stay readable behind it.
    let mut painted = Painted {
        card: render_card(frame, area, state),
    };
    if state.help {
        render_help(frame, area);
    }
    if state.sessions.is_some() {
        render_sessions(frame, area, state);
    }
    if state.login.is_some() {
        render_login(frame, area, state);
    }
    if state.prompt.is_some() {
        render_prompt(frame, area, state);
    }
    if state.quit_prompt {
        render_quit(frame, area);
    }
    // An overlay that covers the sentence has un-shown it, whatever was
    // drawn underneath: the arming rule is about what the user can read.
    if state.help
        || state.sessions.is_some()
        || state.login.is_some()
        || state.prompt.is_some()
        || state.quit_prompt
    {
        painted.card = false;
    }
    painted
}

// ------------------------------------------------------------------- header

fn header_line(state: &State) -> Paragraph<'_> {
    let (glyph, label) = listen_label(state.listen);
    let mut spans = vec![
        Span::styled(
            state.settings.identity.name.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    // While dictating, the header *is* the level meter: the user needs to see
    // that the microphone is actually hearing them before they stop talking.
    if state.dictating {
        spans.push(Span::styled(
            "● LISTENING ",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            meter(state.level),
            Style::new().fg(Color::Red),
        ));
        spans.push(Span::styled("  v stops", Style::new().fg(GREY)));
    } else {
        spans.push(Span::styled(
            format!("{glyph} {label}"),
            Style::new().fg(GREY),
        ));
        match state.mic_device.as_deref() {
            Some(device) => spans.push(Span::raw(format!("  {device}"))),
            None => spans.push(Span::styled("  v dictates", Style::new().fg(GREY))),
        }
    }
    if state.typed_only() {
        spans.push(Span::styled(
            "  NO VOICE OUT",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled("   ", Style::new().fg(GREY)));
    let active = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let idle = Style::new().fg(GREY);
    if state.view == View::Panes && state.focus == Pane::Conversation {
        spans.push(Span::styled("chat", active));
        spans.push(Span::styled("  /project · /model · /login · /help", idle));
    } else {
        spans.push(Span::styled("Esc/q chat", idle));
        spans.push(Span::styled("  ", idle));
        let page = match (state.view, state.focus, state.settings_section) {
            (View::Settings, _, Some(Section::Models)) => "models",
            (View::Settings, _, Some(Section::Connections)) => "login",
            (View::Settings, _, _) => "settings",
            (_, Pane::Projects, _) => "projects",
            (_, Pane::Runs, _) => "runs",
            (_, Pane::Mind, _) if state.selected_run.is_some() => "mind",
            (_, Pane::Mind, _) => "activity",
            _ => "chat",
        };
        spans.push(Span::styled(page, active));
    }
    Paragraph::new(Line::from(spans))
}

// -------------------------------------------------------------------- panes

/// The conversation is the home surface. Reference and configuration pages
/// replace it only after an explicit slash command.
fn render_panes(frame: &mut Frame, area: Rect, state: &State) {
    render_pane(frame, area, state, state.focus);
}

/// What a pane's border says it is holding.
fn pane_title(state: &State, pane: Pane) -> String {
    match pane {
        Pane::Conversation => match state.conversation_title.as_deref() {
            Some(title) => format!(" Conversation · {title} "),
            None => " Conversation ".to_owned(),
        },
        Pane::Runs => {
            let live = state.runs.iter().filter(|run| run.is_live()).count();
            format!(" Runs ({}/{} live) ", live, state.runs.len())
        }
        // The Mind pane is a trace when a run is selected and the raw event
        // ring when none is, and the title has to say which, or a user
        // reading `nav` lines under a `Mind` border cannot tell whose they
        // are.
        Pane::Mind => match state.selected_run() {
            Some(run) => format!(" Mind · {} · {} ", run.kind.label(), run.state.word()),
            None => format!(" Activity ({}) ", state.activity.len()),
        },
        Pane::Projects => match state.projects.get(state.project_row) {
            Some(project) if state.project_detail.is_some() => {
                format!(" Project · {} ", project.name)
            }
            _ => format!(" Projects ({}) ", state.projects.len()),
        },
    }
}

fn render_pane(frame: &mut Frame, area: Rect, state: &State, pane: Pane) {
    // The Conversation is read, not inspected: no border, so the full terminal
    // width goes to the text and a paragraph is not re-wrapped four words at a
    // time. The reference pages keep a frame, because their title carries
    // counts a reader wants.
    let inner = if pane == Pane::Conversation {
        area
    } else {
        let block = Block::bordered()
            .border_style(Style::new().fg(GREY))
            .title(pane_title(state, pane));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    };

    // The Runs and Mind panes render newest-first, so row 0 is already the
    // newest and ratatui's own wrapping is fine. The Conversation reads
    // oldest-first like a transcript and has to be anchored to its *bottom*,
    // which means knowing how many rows it really occupies — so it is
    // wrapped here instead, at the width it will be drawn at, and the tail
    // is what gets rendered. Guessing the row count from `Line::width` is
    // not good enough: undercount and the newest message falls off the
    // bottom of the pane, which is the one line that must always be there.
    let follow = if state.follows(pane) { "▼ live" } else { "" };
    if pane == Pane::Conversation {
        let lines = conversation_lines(state, inner.width);
        let height = usize::from(inner.height);
        // `scroll` counts rows back from the newest.
        // Clamped here, not in the reducer: only the renderer knows how many
        // rows the text actually wrapped to at this width.
        let back = usize::from(state.scroll_of(pane)).min(lines.len().saturating_sub(1));
        let end = lines.len().saturating_sub(back);
        let start = end.saturating_sub(height);
        let window = lines.get(start..end).unwrap_or(&[]).to_vec();
        frame.render_widget(Paragraph::new(window), inner);
    } else {
        let body = match pane {
            Pane::Runs => runs_lines(state),
            Pane::Mind => mind_lines(state),
            Pane::Projects => project_lines(state),
            Pane::Conversation => Vec::new(),
        };
        frame.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .scroll((state.scroll_of(pane), 0)),
            inner,
        );
    }
    if !follow.is_empty() && inner.height > 0 && inner.width >= 6 {
        let marker = Rect::new(inner.x + inner.width - 6, inner.y + inner.height - 1, 6, 1);
        frame.render_widget(
            Paragraph::new(Line::styled(follow, Style::new().fg(GREY))),
            marker,
        );
    }
}

fn project_lines(state: &State) -> Vec<Line<'static>> {
    let Some(selected) = state.projects.get(state.project_row) else {
        return vec![
            Line::styled("No projects yet.", Style::new().fg(GREY)),
            Line::styled(
                "Create one with `neo projects add \"Name\"`, then reopen /project.",
                Style::new().fg(GREY),
            ),
        ];
    };
    if let Some((documents, ticks)) = &state.project_detail {
        let control = |row: usize, label: String| {
            let selected = row == state.project_detail_row;
            Line::styled(
                format!("{} {label}", if selected { "▸" } else { " " }),
                if selected {
                    Style::new().fg(Color::Cyan).bold()
                } else {
                    Style::new()
                },
            )
        };
        let mut lines = vec![
            Line::styled(selected.name.clone(), Style::new().fg(Color::Cyan).bold()),
            Line::styled(selected.root.clone(), Style::new().fg(GREY)),
            Line::raw(""),
            Line::styled("Project settings", Style::new().bold()),
            control(
                0,
                format!(
                    "Run automatically        {}",
                    if selected.heartbeat_enabled {
                        "on"
                    } else {
                        "off"
                    }
                ),
            ),
            control(
                1,
                format!(
                    "Every (seconds)         {}",
                    selected.heartbeat_every_seconds
                ),
            ),
            control(2, format!("When a gate appears     {:?}", selected.on_gate)),
            Line::raw(""),
            Line::styled("Files", Style::new().bold()),
            control(3, "soul.md                  edit in $EDITOR".to_owned()),
            Line::styled(
                format!("    {}", project_excerpt(&documents.soul)),
                Style::new().fg(GREY),
            ),
            control(4, "heartbeat.md             edit in $EDITOR".to_owned()),
            Line::styled(
                format!("    {}", project_excerpt(&documents.heartbeat)),
                Style::new().fg(GREY),
            ),
            Line::raw(""),
            Line::styled("Actions", Style::new().bold()),
            control(5, "Run heartbeat now".to_owned()),
            Line::raw(""),
            Line::styled("Recent activity", Style::new().bold()),
        ];
        lines.extend(ticks.iter().take(5).map(|tick| {
            Line::raw(format!(
                "{}  {:?}  {}",
                tick.started_at,
                tick.outcome,
                tick.reason.as_deref().unwrap_or("")
            ))
        }));
        lines.push(Line::styled(
            "j/k moves · Enter changes or opens · Backspace/← index · Esc/q chat",
            Style::new().fg(GREY),
        ));
        return lines;
    }
    let mut lines = vec![Line::styled(
        "Select a project and press Enter.",
        Style::new().fg(GREY),
    )];
    lines.push(Line::raw(""));
    for (index, project) in state.projects.iter().enumerate() {
        let cursor = if index == state.project_row {
            "▸"
        } else {
            " "
        };
        let clock = if project.heartbeat_enabled {
            format!("every {}s", project.heartbeat_every_seconds)
        } else {
            "off".to_owned()
        };
        lines.push(Line::styled(
            format!("{cursor} {}", project.name),
            if index == state.project_row {
                Style::new().fg(Color::Cyan).bold()
            } else {
                Style::new()
            },
        ));
        lines.push(Line::styled(
            format!("  {} · {clock}", project.slug),
            Style::new().fg(GREY),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "j/k moves · Enter opens · Esc/q chat",
        Style::new().fg(GREY),
    ));
    lines
}

fn project_excerpt(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    let mut excerpt = first.chars().take(80).collect::<String>();
    if first.chars().count() > 80 {
        excerpt.push('…');
    }
    excerpt
}

/// The conversation, wrapped to `width`: the thread, then whatever the
/// running turn is doing.
///
/// Wrapped here rather than by `Wrap`, because the pane is anchored to its
/// newest line and that needs an exact row count. The gutter is preserved
/// on continuation rows, so a long answer stays visually one block instead
/// of running back under the `you`/`neo` labels.
///
/// A turn in flight shows one card per action — intent, reason, newest
/// detail line, and how it ended — and the answer as the model writes it.
/// The point is that the user can see it working, and can read the reply
/// before the turn is over.
fn conversation_lines(state: &State, width: u16) -> Vec<Line<'static>> {
    if state.thread.is_empty() && state.turn.is_none() {
        return vec![
            Line::styled(
                "Ask Starkbot to do something in an app or on the web.",
                Style::new().fg(GREY),
            ),
            Line::styled(
                "Type a message and press Enter · / opens commands",
                Style::new().fg(GREY),
            ),
        ];
    }
    let mut lines = Vec::with_capacity(state.thread.len() * 2 + 8);
    for message in &state.thread {
        let (label, style) = match message.role {
            // A steered message is marked in the gutter: it reached a turn
            // that was already running, so the answer above it may already
            // account for it. Without the arrow the transcript reads as if
            // the model replied before it was asked.
            Role::User if message.steered => ("you↵", Style::new().fg(Color::Cyan).bold()),
            Role::User => ("you", Style::new().fg(Color::Cyan).bold()),
            Role::Assistant => ("neo", Style::new().fg(Color::White).bold()),
            // A tool result is what an action produced: dimmer, because it is
            // evidence rather than conversation.
            Role::Tool => ("·", Style::new().fg(GREY)),
        };
        lines.extend(wrapped(
            &format!("{label:<4}"),
            style,
            &message.text,
            Style::new(),
            width,
        ));
    }
    if let Some(turn) = state.turn.as_ref() {
        lines.push(Line::raw(""));
        for card in &turn.cards {
            lines.extend(card_lines(card, width));
        }
        // Only until there is something to show. A turn that answers
        // straight out publishes text before it publishes anything else,
        // and "thinking" sitting above a reply that is already arriving
        // is a spinner pretending the work has not started.
        if turn.cards.is_empty() && turn.answer.is_empty() {
            lines.push(Line::styled("▸   thinking", Style::new().fg(Color::Yellow)));
        }
        // The answer as it arrives, under the same `neo` gutter the finished
        // message will use, so nothing jumps when the turn ends and the
        // stored row takes its place.
        if !turn.answer.is_empty() {
            lines.extend(wrapped(
                "neo ",
                Style::new().fg(Color::White).bold(),
                &turn.answer,
                Style::new(),
                width,
            ));
        }
    }
    lines
}

/// One tool card: the action, why, the newest line from inside it, and how
/// it ended once it has.
///
/// Styled through [`run_style`] and [`TraceKind`]'s colours rather than a
/// palette of its own, so a card and the Mind pane's trace of the same step
/// are read the same way.
fn card_lines(card: &StepCard, width: u16) -> Vec<Line<'static>> {
    let style = run_style(&card.state);
    let mut lines = wrapped(
        &format!("{}   ", card.state.glyph()),
        style,
        &card.intent,
        style.add_modifier(Modifier::BOLD),
        width,
    );
    if let Some(thought) = card.thought.as_ref() {
        lines.extend(wrapped(
            "    ",
            Style::new(),
            thought,
            Style::new().fg(trace_colour(TraceKind::Thought)),
            width,
        ));
    }
    if let Some(detail) = card.detail.as_ref() {
        let kind = if card.state == RunState::Running {
            TraceKind::Decision
        } else {
            TraceKind::Observation
        };
        lines.extend(wrapped(
            "    ",
            Style::new(),
            detail,
            Style::new().fg(trace_colour(kind)),
            width,
        ));
    }
    // The closing line carries the word as well as the colour, so a
    // monochrome terminal still reads "failed" (14 §2).
    if card.state != RunState::Running {
        let took = card
            .duration_ms
            .map_or_else(String::new, |ms| format!(" · {ms} ms"));
        lines.push(Line::styled(
            format!("    {}{took}", card.state.word()),
            style,
        ));
    }
    lines
}

/// `text` broken to fit beside a fixed gutter, one [`Line`] per screen row.
///
/// Greedy on whitespace, and a word longer than the column — a URL, a bundle
/// id — is split rather than allowed to overflow, because a row wider than
/// the pane is silently truncated by the backend and the tail is simply lost.
fn wrapped(
    gutter: &str,
    gutter_style: Style,
    text: &str,
    style: Style,
    width: u16,
) -> Vec<Line<'static>> {
    let gutter_width = gutter.chars().count();
    let column = usize::from(width).saturating_sub(gutter_width).max(1);
    let blank = " ".repeat(gutter_width);
    let mut rows: Vec<String> = Vec::new();
    // A message may already contain newlines; each is its own paragraph.
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split(' ') {
            for piece in split_word(word, column) {
                let needed = if current.is_empty() {
                    piece.chars().count()
                } else {
                    current.chars().count() + 1 + piece.chars().count()
                };
                if needed > column && !current.is_empty() {
                    rows.push(std::mem::take(&mut current));
                } else if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(&piece);
            }
        }
        rows.push(current);
    }
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let lead = if index == 0 { gutter } else { &blank };
            Line::from(vec![
                Span::styled(lead.to_owned(), gutter_style),
                Span::styled(row, style),
            ])
        })
        .collect()
}

/// One word, in pieces no wider than `column`.
fn split_word(word: &str, column: usize) -> Vec<String> {
    if word.chars().count() <= column {
        return vec![word.to_owned()];
    }
    let characters: Vec<char> = word.chars().collect();
    characters
        .chunks(column)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Everything this front end started, newest first.
///
/// Newest first because the one a user is watching is the one they just
/// asked for, and a list that grows downwards pushes it off the pane.
fn runs_lines(state: &State) -> Vec<Line<'_>> {
    if state.runs.is_empty() {
        return vec![
            Line::styled("nothing started yet.", Style::new().fg(GREY)),
            Line::styled(
                "Enter sends a turn · :nav · :app · :ax · :eval",
                Style::new().fg(GREY),
            ),
        ];
    }
    let selected = state.selected_run;
    let mut lines = Vec::with_capacity(state.runs.len() * 2);
    for run in state.runs.iter().rev() {
        let chosen = selected == Some(run.id);
        let marker = if chosen { "▸" } else { " " };
        let style = run_style(&run.state);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{} ", run.state.glyph()),
                Style::new().fg(Color::Cyan),
            ),
            Span::styled(format!("{:<5}", run.kind.label()), style),
            Span::styled(
                format!("{:<9}", run.state.word()),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>6}  {} step(s)", run.elapsed(state.now_ms), run.steps),
                Style::new().fg(GREY),
            ),
        ]));
        lines.push(Line::styled(
            format!("   {}", run.title),
            if chosen {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            },
        ));
        if !run.last.is_empty() {
            lines.push(Line::styled(
                format!("   {}", run.last),
                Style::new().fg(GREY),
            ));
        }
    }
    lines
}

fn run_style(state: &RunState) -> Style {
    match state {
        RunState::Running => Style::new().fg(Color::Blue),
        RunState::Stopping => Style::new().fg(Color::Yellow),
        RunState::Done => Style::new().fg(Color::Green),
        RunState::Failed(_) => Style::new().fg(Color::Red),
        RunState::Cancelled => Style::new().fg(GREY),
    }
}

/// The selected run's trace, or the raw event ring when nothing is selected.
fn mind_lines(state: &State) -> Vec<Line<'_>> {
    match state.selected_run() {
        Some(run) => trace_lines(run),
        None => activity_lines(state),
    }
}

fn trace_lines(run: &Run) -> Vec<Line<'_>> {
    if run.trace.is_empty() {
        return vec![Line::styled(
            "no trace yet — the first step appears as the model chooses it",
            Style::new().fg(GREY),
        )];
    }
    run.trace
        .iter()
        .rev()
        .map(|entry| {
            Line::from(vec![
                Span::styled(
                    format!("{:<8}", entry.kind.label()),
                    Style::new().fg(trace_colour(entry.kind)),
                ),
                Span::raw(entry.text.clone()),
            ])
        })
        .collect()
}

const fn trace_colour(kind: TraceKind) -> Color {
    match kind {
        TraceKind::Step => Color::Yellow,
        TraceKind::Thought => GREY,
        TraceKind::Observation | TraceKind::Summary => Color::White,
        TraceKind::Note | TraceKind::Launch => Color::Blue,
        TraceKind::Decision => Color::Magenta,
        TraceKind::Outcome | TraceKind::Result => Color::Green,
        TraceKind::Case => Color::Cyan,
    }
}

fn activity_lines(state: &State) -> Vec<Line<'_>> {
    if state.activity.is_empty() {
        return vec![Line::styled(
            "no events yet — this ring fills as the core emits AppEvents",
            Style::new().fg(GREY),
        )];
    }
    state
        .activity
        .iter()
        .rev()
        .map(|entry| {
            Line::from(vec![
                Span::styled(format!("{:<10}", entry.kind), Style::new().fg(Color::Blue)),
                Span::raw(entry.detail.clone()),
            ])
        })
        .collect()
}

// ----------------------------------------------------------------- settings

fn render_settings(frame: &mut Frame, area: Rect, state: &State) {
    let (title, hint) = match state.settings_section {
        Some(Section::Connections) => (
            " Login & connections ",
            " Enter selects · s sets key · x removes · K checks · c signs in · d signs out · Esc/q chat ",
        ),
        Some(Section::Models) => (
            " Model configuration ",
            " Enter edits or cycles · r refreshes · Esc/q chat ",
        ),
        Some(section) => (section.title(), " Enter edits or toggles · Esc/q chat "),
        None => (
            " Settings ",
            " Enter edits/toggles · s key · c login · Esc/q chat ",
        ),
    };
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Cyan))
        .title(title)
        .title_bottom(hint);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = state.rows();
    let lines: Vec<Line<'_>> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| settings_line(row, index == state.row))
        .collect();
    let height = usize::from(inner.height);
    let offset = state.row.saturating_sub(height.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(lines).scroll((u16::try_from(offset).unwrap_or(0), 0)),
        inner,
    );
}

fn settings_line<'a>(row: &'a Row, selected: bool) -> Line<'a> {
    if row.heading {
        return Line::styled(
            format!("── {} ", row.label),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        );
    }
    let marker = if selected { "▸ " } else { "  " };
    let value_style = match row.section {
        Section::Connections => connection_value_style(&row.value),
        _ => Style::new(),
    };
    let label_style = if selected {
        Style::new().add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(GREY)
    };
    Line::from(vec![
        Span::styled(marker, Style::new().fg(Color::Cyan)),
        Span::styled(format!("{:<26}", row.label), label_style),
        Span::styled(row.value.clone(), value_style),
    ])
}

fn connection_value_style(value: &str) -> Style {
    match value.split(' ').next().unwrap_or(value) {
        "present" | "connected" => Style::new().fg(Color::Green),
        "invalid" | "unavailable" => Style::new().fg(Color::Red),
        "missing" | "signed" => Style::new().fg(Color::Yellow),
        _ => Style::new(),
    }
}

// ----------------------------------------------------------- composer/status

fn composer_line(state: &State) -> Paragraph<'_> {
    let draft = state.composer.replace('\n', "⏎");
    let prefix = if state.mode == Mode::Command {
        "/"
    } else {
        "> "
    };
    let typed = if state.mode == Mode::Command {
        state.line.clone()
    } else {
        draft
    };
    let mut spans = vec![
        Span::styled(prefix, Style::new().fg(Color::Cyan)),
        Span::raw(typed.clone()),
    ];
    if typed.is_empty() {
        let hint = match state.mode {
            Mode::Command => "Tab completes · try project, model, login, sessions",
            Mode::Insert => state.composer_hint(),
            Mode::Normal | Mode::Card => "/ opens commands · i starts typing",
        };
        if !hint.is_empty() {
            spans.push(Span::styled(hint, Style::new().fg(GREY)));
        }
    }
    Paragraph::new(Line::from(spans))
}

fn status_line(state: &State, compact_header: bool, width: u16) -> Paragraph<'_> {
    let store = state.store.path.display().to_string();
    let model = &state.settings.models.inference;
    let mut segments = vec![state.mode.label().to_owned()];
    // While a turn runs the status line is about that turn; idle, it is
    // about the machine. It goes first because segments drop right to left
    // on a narrow terminal, and the one thing a user watching an agent work
    // must not lose is how long it has been going.
    if let Some(turn) = state.turn.as_ref() {
        segments.push(turn_segment(state, turn));
    }
    if compact_header {
        let (glyph, label) = listen_label(state.listen);
        segments.push(format!("{glyph} {label}"));
    }
    if let Some(status) = &state.status {
        segments.push(status.clone());
    }
    let live = state.runs.iter().filter(|run| run.is_live()).count();
    if live > 0 {
        segments.push(format!("{live} running · Esc stops"));
    }
    segments.push(format!("{store} v{}", state.store.schema_version));
    segments.push(format!(
        "{} · {}/{}",
        connection_label(state.inference),
        model.provider.as_str(),
        model.id
    ));
    segments.push(key_segment(state));
    segments.push(format!("account {}", account_segment(state)));

    // Segments drop right-to-left rather than being cut mid-word (14 §2).
    let fits = |segments: &[String]| {
        segments
            .iter()
            .map(|part| part.chars().count())
            .sum::<usize>()
            + 3 * segments.len().saturating_sub(1)
            <= usize::from(width)
    };
    while segments.len() > 1 && !fits(&segments) {
        segments.pop();
    }

    let ready = state
        .keys
        .iter()
        .all(|status| status.state == KeyState::Present);
    let style = if ready {
        Style::new().fg(GREY)
    } else {
        Style::new().fg(Color::Yellow)
    };
    Paragraph::new(Line::styled(segments.join(" │ "), style))
}

/// `model · elapsed · N steps · X in / Y out` for the turn in flight.
///
/// The model id without its provider: the provider is already on the
/// connection segment, and this line has to survive a 60-column terminal,
/// where it is the segment a user actually needs.
fn turn_segment(state: &State, turn: &TurnProgress) -> String {
    let elapsed = state
        .runs
        .iter()
        .find(|run| run.id == turn.run)
        .map_or_else(|| "0:00".to_owned(), |run| run.elapsed(state.now_ms));
    // The vendor's counts arrive after a round trip, so the first moments
    // of a turn have nothing to report. Saying so beats printing two zeros
    // for a turn that is demonstrably spending tokens.
    let tokens = turn.usage.map_or_else(
        || "tokens pending".to_owned(),
        |usage| format!("{} in / {} out", usage.input_tokens, usage.output_tokens),
    );
    format!(
        "{} · {elapsed} · {} step(s) · {tokens}",
        state.settings.models.inference.id, turn.done
    )
}

/// Per-account key state, short enough to survive on an 80-column status line:
/// accounts are grouped by state and the healthy group is implied.
fn key_segment(state: &State) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for status in &state.keys {
        if status.state == KeyState::Present {
            continue;
        }
        let label = key_label(status.state);
        match groups.iter_mut().find(|(name, _)| *name == label) {
            Some((_, accounts)) => accounts.push(&status.account),
            None => groups.push((label, vec![&status.account])),
        }
    }
    if groups.is_empty() {
        return "keys present".to_owned();
    }
    let body = groups
        .into_iter()
        .map(|(label, accounts)| format!("{label}: {}", accounts.join(", ")))
        .collect::<Vec<_>>()
        .join(" · ");
    format!("keys {body}")
}

fn account_segment(state: &State) -> String {
    match &state.account {
        Some(account) => format!(
            "{} {}",
            account.provider.as_str(),
            crate::state::account_status_label(account.status)
        ),
        None => "none".to_owned(),
    }
}

// ----------------------------------------------------------------- overlays

fn render_help(frame: &mut Frame, area: Rect) {
    let mut lines: Vec<Line<'_>> = [
        "Chat opens ready to type. Enter sends; / opens the command line.",
        "Esc leaves typing. Esc again stops a live run; q asks before quitting.",
        "On a page, Esc or q returns to chat.",
        "j/k or arrows move · Enter opens or changes · Backspace/← returns to an index",
        "Ctrl-N starts a conversation · Ctrl-V dictates · Ctrl-Q quits now",
        "Ctrl-C kill switch · Ctrl-L redraw · x stops the selected run",
        "Cards: y/n confirms · j/k picks · 1-9 answers · i types a free answer",
        "Login: c signs in · d signs out · s sets a key · x removes · K checks",
        "Models: Enter edits or cycles · r refreshes the catalogue",
        "",
    ]
    .iter()
    .map(|line| Line::raw(*line))
    .collect();
    lines.push(Line::styled(
        "slash commands",
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ));
    for spec in &COMMAND_LINE {
        let mut spans = vec![Span::styled(
            format!("  /{:<9}", spec.name),
            Style::new().fg(Color::Cyan),
        )];
        spans.push(Span::styled(
            format!("{:<46}", spec.args),
            Style::new().fg(GREY),
        ));
        spans.push(match spec.unavailable {
            Some(_) => Span::styled(
                format!("{} — not built yet", spec.help),
                Style::new().fg(Color::Yellow),
            ),
            None => Span::raw(spec.help.to_owned()),
        });
        lines.push(Line::from(spans));
    }

    let block = Block::bordered()
        .title(" Help ")
        .border_style(Style::new().fg(Color::Cyan));
    let height = u16::try_from(lines.len()).unwrap_or(8) + 2;
    let rect = centered(area, 104, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The conversation switcher.
fn render_sessions(frame: &mut Frame, area: Rect, state: &State) {
    let Some(sessions) = state.sessions.as_ref() else {
        return;
    };
    let lines: Vec<Line<'_>> = if sessions.rows.is_empty() {
        vec![Line::styled(
            "no conversations yet — Ctrl-N starts one",
            Style::new().fg(GREY),
        )]
    } else {
        sessions
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let selected = index == sessions.row;
                let marker = if selected { "▸ " } else { "  " };
                let title = Style::new();
                Line::from(vec![
                    Span::styled(marker, Style::new().fg(Color::Cyan)),
                    Span::styled(
                        format!("{:<40}", row.title),
                        if selected {
                            title.add_modifier(Modifier::BOLD)
                        } else {
                            title
                        },
                    ),
                    Span::styled(format!("{:<22}", row.when), Style::new().fg(GREY)),
                    Span::styled(
                        if row.active { "open" } else { "" },
                        Style::new().fg(Color::Green),
                    ),
                ])
            })
            .collect()
    };
    let block = Block::bordered()
        .title(" Conversations ")
        .title_bottom(" j k moves · Enter opens · Ctrl-N new · /rename retitles · Esc closes ")
        .border_style(Style::new().fg(Color::Cyan));
    let height = u16::try_from(lines.len()).unwrap_or(4).min(20) + 2;
    let rect = centered(area, 80, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The subscription login overlay (K7).
///
/// Everything on screen is a public value: the plan name, the authorize URL,
/// the loopback redirect URI. The authorization code and the tokens are
/// exchanged inside the core and never reach this module.
fn render_login(frame: &mut Frame, area: Rect, state: &State) {
    let Some(login) = state.login.as_ref() else {
        return;
    };
    let mut lines = vec![
        Line::from(vec![
            Span::raw("Sign in to "),
            Span::styled(login.title, Style::new().fg(Color::Cyan).bold()),
            Span::raw(" in your browser."),
        ]),
        Line::raw(""),
        Line::styled(login.url.clone(), Style::new().fg(Color::Blue)),
        Line::raw(""),
    ];
    match &login.phase {
        LoginPhase::Waiting => {
            lines.push(Line::from(vec![
                Span::styled("waiting for your browser", Style::new().fg(Color::Yellow)),
                Span::raw(format!(" · {}s left", login.remaining)),
            ]));
            lines.push(Line::raw(format!("listening on {}", login.redirect_uri)));
        }
        LoginPhase::Pasting => {
            lines.push(Line::raw(
                "Paste the URL your browser was redirected to, then press Enter:",
            ));
            // A redirect URL is not a secret, so unlike a key prompt this is
            // shown — the user needs to see that the paste arrived intact.
            lines.push(Line::styled(
                truncate_tail(&login.paste, 68),
                Style::new().fg(Color::White).bold(),
            ));
        }
        LoginPhase::Exchanging => lines.push(Line::styled(
            "exchanging the code…",
            Style::new().fg(Color::Yellow),
        )),
        LoginPhase::Done => lines.push(Line::styled(
            "connected",
            Style::new().fg(Color::Green).bold(),
        )),
        LoginPhase::Failed(detail) => {
            lines.push(Line::styled(
                format!("failed: {detail}"),
                Style::new().fg(Color::Red),
            ));
            lines.push(Line::raw(
                "p pastes the redirect URL instead · o opens the page again",
            ));
        }
    }
    let hint = match login.phase {
        LoginPhase::Pasting => " Enter finishes · Esc cancels ",
        LoginPhase::Done => " Esc closes ",
        _ => " o opens the page · p pastes instead · Esc cancels ",
    };
    let block = Block::bordered()
        .title(format!(" {} ", login.title))
        .title_bottom(hint)
        .border_style(Style::new().fg(Color::Cyan));
    let height = u16::try_from(lines.len()).unwrap_or(8) + 2;
    let rect = centered(area, 76, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// Keep the end of a long pasted URL visible: the code and state at the tail
/// are what the user is checking arrived.
fn truncate_tail(value: &str, width: usize) -> String {
    let count = value.chars().count();
    if count <= width {
        return value.to_owned();
    }
    let skip = count - width.saturating_sub(1);
    format!("…{}", value.chars().skip(skip).collect::<String>())
}

/// The card: what is about to happen, where, and the keys that answer it
/// (04 §13, 16 §5.5).
///
/// Says whether the sentence reached the frame, which is what arms `y` and
/// `n`. Nothing is drawn at all when the overlay would not fit: half a
/// sentence about money is worse than no sentence, and an undrawn card is
/// one no keystroke can resolve.
fn render_card(frame: &mut Frame, area: Rect, state: &State) -> bool {
    let Some(card) = state.card.as_ref() else {
        return false;
    };
    let width = CARD_WIDTH.min(area.width);
    // The block's own border takes a column either side.
    let column = width.saturating_sub(2);
    let confirm = matches!(card.kind, CardKind::Confirm { .. });

    // The sentence, in full. It wraps rather than being cut, and a word
    // wider than the overlay is split, so nothing reads as a shorter claim
    // than the core made.
    let mut lines = wrapped(
        "",
        Style::new(),
        &card.sentence,
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        column,
    );
    let sentence_rows = lines.len();
    // One blank line between blocks, and none around a block that is not
    // there: an ask carries no context rows, and two blank lines where they
    // would have been reads as something failed to load.
    for (label, value) in [
        ("where ", card.context.as_deref()),
        ("why   ", card.cause.as_deref()),
        ("cost  ", card.cost.as_deref()),
    ] {
        if let Some(value) = value {
            if lines.len() == sentence_rows {
                lines.push(Line::raw(""));
            }
            lines.extend(wrapped(
                label,
                Style::new().fg(GREY),
                value,
                Style::new(),
                column,
            ));
        }
    }
    if !card.options().is_empty() {
        lines.push(Line::raw(""));
        for (index, option) in card.options().iter().enumerate() {
            let chosen = index == card.selected();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} {} ", index + 1, if chosen { "▸" } else { " " }),
                    Style::new().fg(Color::Cyan),
                ),
                Span::styled(
                    option.clone(),
                    if chosen {
                        Style::new().add_modifier(Modifier::BOLD)
                    } else {
                        Style::new()
                    },
                ),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines.push(card_keys(card));
    if !state.queued_cards.is_empty() {
        lines.push(Line::styled(
            format!("{} more waiting behind this one", state.queued_cards.len()),
            Style::new().fg(GREY),
        ));
    }

    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    if height > area.height || width < CARD_MIN_WIDTH {
        return false;
    }
    let (title, hint, colour) = if confirm {
        (
            " Confirm ",
            " y approves · n denies · Enter is not an answer ",
            Color::Yellow,
        )
    } else if card.free_text() {
        (
            " Question ",
            " i types the answer · the card stays until it is answered ",
            Color::Cyan,
        )
    } else {
        (
            " Question ",
            " j k moves · 1-9 answers · y sends the highlighted one ",
            Color::Cyan,
        )
    };
    let block = Block::bordered()
        .title(title)
        .title_bottom(hint)
        .border_style(Style::new().fg(colour));
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
    true
}

/// The keys line: what this card can be answered with *now*.
///
/// Before the debounce elapses it says so instead of offering keys that do
/// nothing — a binding that is advertised and inert is worse than one that
/// is visibly not ready yet.
fn card_keys(card: &Card) -> Line<'static> {
    if card.free_text() {
        return Line::from(vec![
            Span::styled("[i]", Style::new().fg(Color::Cyan)),
            Span::raw(" type your answer"),
        ]);
    }
    if !card.live() {
        return Line::styled("reading…", Style::new().fg(GREY));
    }
    if matches!(card.kind, CardKind::Confirm { .. }) {
        let mut spans = vec![
            Span::styled("[y]", Style::new().fg(Color::Green)),
            Span::raw(" yes   "),
            Span::styled("[n]", Style::new().fg(Color::Red)),
            Span::raw(" no"),
        ];
        // Q2 approvals are single-shot, so the affordance is not offered at
        // all rather than offered and refused.
        if card.can_remember {
            spans.push(Span::styled("   [r]", Style::new().fg(Color::Cyan)));
            spans.push(Span::raw(" remember this"));
        }
        return Line::from(spans);
    }
    Line::from(vec![
        Span::styled("[y]", Style::new().fg(Color::Green)),
        Span::raw(" send the highlighted answer"),
    ])
}

fn render_prompt(frame: &mut Frame, area: Rect, state: &State) {
    let Some(prompt) = state.prompt.as_ref() else {
        return;
    };
    // A masked prompt renders one bullet per character and never the buffer.
    let shown = if prompt.masked {
        "•".repeat(prompt.len())
    } else {
        prompt.visible().to_owned()
    };
    let block = Block::bordered()
        .title(format!(" {} ", prompt.label))
        .title_bottom(format!(" {} ", prompt.hint))
        .border_style(Style::new().fg(Color::Yellow));
    let rect = centered(area, 66, 3);
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(Line::raw(shown)).block(block), rect);
}

fn render_quit(frame: &mut Frame, area: Rect) {
    let block = Block::bordered().border_style(Style::new().fg(Color::Yellow));
    let rect = centered(area, 46, 3);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(Line::raw("quit neo tui?  [y] yes   [n] no")).block(block),
        rect,
    );
}

/// A twenty-cell peak meter. Public because the render tests assert on it.
fn meter(level: f32) -> String {
    let filled = ((level.clamp(0.0, 1.0) * 20.0).round() as usize).min(20);
    let mut bar = String::with_capacity(20);
    for cell in 0..20 {
        bar.push(if cell < filled { '▇' } else { '·' });
    }
    bar
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}
