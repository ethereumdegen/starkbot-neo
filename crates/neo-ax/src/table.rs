//! Building the element table: pruning, reading order, budgets, truncation.
//!
//! Everything here is a pure function of a `RawNode` tree plus the menu-bar
//! leaves, so the whole of `plans/01-accessibility.md` §Element table is
//! testable with no Mac UI.

use crate::mapping::{
    carries_text, display_role, is_busy, is_decoration, is_modal, is_table_worthy, is_tunnel,
    operations, state_of,
};
use crate::raw::{MenuLeaf, RawNode, collapse_whitespace, elide};
use crate::types::{
    AppInfo, Control, Element, ElementTable, Fingerprint, Operation, Rect, Ref, WindowInfo,
};

/// Hard ceiling on rows (A3).
pub(crate) const MAX_ELEMENTS: usize = 250;
/// The most of the element budget any one `(role, container)` group may take,
/// as a percentage. At 40% a grid can still dominate — it should, it is what
/// the user is looking at — but it cannot leave zero room for anything else.
pub(crate) const GROUP_SHARE_PERCENT: usize = 40;
/// Window elements take this many before menus get their share.
pub(crate) const WINDOW_CAP: usize = 170;
/// Menu leaves are guaranteed at least this many slots when there are any.
pub(crate) const MENU_MIN: usize = MAX_ELEMENTS - WINDOW_CAP;
/// Visible text budget.
pub(crate) const TEXT_BUDGET: usize = 6_000;
/// Per-value budget.
pub(crate) const VALUE_MAX: usize = 80;
/// Depth budget, counted after tunnelling.
pub(crate) const DEPTH_BUDGET: usize = 25;

/// Which way the governing scroll area can still move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScrollAffordance {
    /// Content exists above the viewport.
    pub up: bool,
    /// Content exists below the viewport.
    pub down: bool,
}

/// Everything the builder needs. Borrowed: it copies only what it emits.
pub(crate) struct TableInput<'a> {
    /// Generation these indices belong to.
    pub generation: u32,
    /// The observed app.
    pub app: AppInfo,
    /// The focused window's subtree.
    pub window: &'a RawNode,
    /// Menu-bar leaves, already flattened.
    pub menu: &'a [MenuLeaf],
    /// Goal text, used only to rank menu leaves.
    pub goal: Option<&'a str>,
    /// What the governing scroll area can do right now.
    pub scroll: ScrollAffordance,
    /// Whether the last settle hit its cap.
    pub settle_timed_out: bool,
    /// `AXURL` of a web area in the window.
    pub url: Option<String>,
}

/// The table plus the bookkeeping the actor keeps on its own thread.
pub(crate) struct BuiltTable {
    /// What the caller sees.
    pub table: ElementTable,
    /// Element index -> `RawNode::id`, i.e. a slot in the actor's element store.
    pub raw_ids: Vec<u32>,
    /// Element index -> fingerprint, for relocation and guards.
    pub fingerprints: Vec<Fingerprint>,
}

struct Candidate<'a> {
    node: &'a RawNode,
    container: Option<String>,
    path: Vec<u16>,
    on_screen: bool,
}

/// Build the element table for one observation.
pub(crate) fn build_table(input: TableInput<'_>) -> BuiltTable {
    // A modal sheet or dialog replaces the window's elements: nothing behind a
    // modal is offered.
    let (root, modal) = match find_modal(input.window) {
        Some(sheet) => (sheet, true),
        None => (input.window, false),
    };

    let window_frame = if root.frame.is_visible_size() {
        root.frame
    } else {
        input.window.frame
    };

    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    let mut text = String::new();
    let mut text_truncated = false;
    let mut busy = false;

    collect(
        root,
        &mut Collect {
            depth: 0,
            path: Vec::new(),
            container: None,
            window_frame,
            out: &mut candidates,
            text: &mut text,
            text_truncated: &mut text_truncated,
            busy: &mut busy,
        },
    );

    // On-screen first, then off-screen, each already in reading order.
    let mut ordered: Vec<Candidate<'_>> = Vec::with_capacity(candidates.len());
    let (onscreen, offscreen): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|c| c.on_screen);
    ordered.extend(onscreen);
    ordered.extend(offscreen);
    // How many the window really offered, kept before the budget is applied:
    // `truncated` has to describe the surface, not the list that survived.
    let offered = ordered.len();
    // The ceiling, not the window share: the menu-slack calculation below
    // decides the final window take, and truncating an already-spread list
    // keeps it diverse.
    ordered = spread(ordered, MAX_ELEMENTS);

    let menu_ranked = rank_menu(input.menu, input.goal);
    let window_cap = WINDOW_CAP.max(MAX_ELEMENTS.saturating_sub(menu_ranked.len()));
    let window_take = ordered.len().min(window_cap);
    let menu_cap = MAX_ELEMENTS
        .saturating_sub(window_take)
        .max(if menu_ranked.is_empty() {
            0
        } else {
            MENU_MIN.min(menu_ranked.len())
        });
    let menu_take = menu_ranked.len().min(menu_cap);

    let truncated = text_truncated || offered > window_take || menu_ranked.len() > menu_take;

    let mut elements = Vec::with_capacity(window_take + menu_take);
    let mut raw_ids = Vec::with_capacity(window_take + menu_take);
    let mut fingerprints = Vec::with_capacity(window_take + menu_take);

    for candidate in ordered.into_iter().take(window_take) {
        let index = u16::try_from(elements.len()).unwrap_or(u16::MAX);
        let node = candidate.node;
        elements.push(Element {
            index,
            role: display_role(&node.role, node.subrole.as_deref()),
            label: node.label(),
            value: element_value(node),
            state: state_of(node),
            container: candidate.container.clone(),
            operations: operations(node, false),
            options: node.options.clone(),
            r: Ref {
                generation: input.generation,
                index,
            },
            frame: node.frame,
        });
        raw_ids.push(node.id);
        fingerprints.push(Fingerprint {
            role: node.role.clone(),
            subrole: node.subrole.clone(),
            label: node.label(),
            identifier: node.identifier.clone(),
            path: candidate.path,
            container: candidate.container,
        });
    }

    for leaf in menu_ranked.into_iter().take(menu_take) {
        let index = u16::try_from(elements.len()).unwrap_or(u16::MAX);
        elements.push(Element {
            index,
            role: "menuitem".to_owned(),
            label: leaf.label(),
            value: leaf.shortcut.clone(),
            state: crate::types::State {
                enabled: leaf.enabled,
                ..Default::default()
            },
            container: Some("menu bar".to_owned()),
            operations: if leaf.enabled {
                vec![Operation::Menu]
            } else {
                Vec::new()
            },
            options: Vec::new(),
            r: Ref {
                generation: input.generation,
                index,
            },
            frame: Rect::default(),
        });
        raw_ids.push(leaf.id);
        fingerprints.push(Fingerprint {
            role: "AXMenuItem".to_owned(),
            subrole: None,
            label: leaf.label(),
            identifier: None,
            path: Vec::new(),
            container: Some("menu bar".to_owned()),
        });
    }

    let mut controls = Vec::new();
    if input.scroll.up {
        controls.push(Control::ScrollUp);
    }
    if input.scroll.down {
        controls.push(Control::ScrollDown);
    }
    if busy || input.settle_timed_out {
        controls.push(Control::Wait);
    }

    let window_title = window_title_of(input.window, root, modal);
    let window_fingerprint = window_fingerprint(input.window);

    BuiltTable {
        table: ElementTable {
            generation: input.generation,
            app: input.app,
            window: WindowInfo {
                title: window_title,
                modal,
                url: input.url,
                frame: window_frame,
                fingerprint: window_fingerprint,
            },
            text,
            elements,
            controls,
            truncated,
        },
        raw_ids,
        fingerprints,
    }
}

fn window_title_of(window: &RawNode, root: &RawNode, modal: bool) -> String {
    if modal {
        let sheet = root.label();
        let base = window.label();
        return match (sheet.is_empty(), base.is_empty()) {
            (false, false) => format!("{base} — {sheet}"),
            (false, true) => sheet,
            _ => base,
        };
    }
    window.label()
}

/// The sheet or modal dialog attached to the window, if there is one.
///
/// Direct children only: sheets and modal dialogs attach to a window, and a
/// separate modal window is a different focused window rather than a sheet.
/// The guard re-runs exactly this check on the live tree, so the two agree.
pub(crate) fn find_modal(node: &RawNode) -> Option<&RawNode> {
    if is_modal(node) {
        return Some(node);
    }
    node.children.iter().find(|child| is_modal(child))
}

/// Role-and-title fingerprint of a window, recomputed by the guard.
pub(crate) fn window_fingerprint(window: &RawNode) -> String {
    format!(
        "{}|{}",
        display_role(&window.role, window.subrole.as_deref()),
        window.label()
    )
}

struct Collect<'a, 'n> {
    depth: usize,
    path: Vec<u16>,
    container: Option<String>,
    window_frame: Rect,
    out: &'a mut Vec<Candidate<'n>>,
    text: &'a mut String,
    text_truncated: &'a mut bool,
    busy: &'a mut bool,
}

fn collect<'n>(node: &'n RawNode, ctx: &mut Collect<'_, 'n>) {
    if is_decoration(node) {
        return;
    }
    if is_busy(node) {
        *ctx.busy = true;
    }

    if let Some(text) = visible_text(node) {
        push_text(ctx.text, &text, ctx.text_truncated);
    }

    let tunnelled = is_tunnel(node);
    if !tunnelled && is_table_worthy(node) && node.frame.is_visible_size() {
        ctx.out.push(Candidate {
            node,
            container: ctx.container.clone(),
            path: ctx.path.clone(),
            on_screen: node.frame.intersects(&ctx.window_frame),
        });
    }

    // Anonymous wrappers cost no depth, so a budget of 25 still reaches the
    // deep DOM of an Electron or Safari window.
    let child_depth = if tunnelled { ctx.depth } else { ctx.depth + 1 };
    if child_depth > DEPTH_BUDGET {
        return;
    }

    let child_container = container_label(node, &ctx.container);
    let mut row_number = 0u16;
    for (i, child) in node.children.iter().enumerate() {
        let is_row = matches!(child.role.as_str(), "AXRow" | "AXOutlineRow");
        if is_row {
            row_number += 1;
        }
        let mut child_ctx = Collect {
            depth: child_depth,
            path: {
                let mut p = ctx.path.clone();
                p.push(u16::try_from(i).unwrap_or(u16::MAX));
                p
            },
            container: if is_row {
                Some(format!("row {row_number}"))
            } else {
                child_container.clone()
            },
            window_frame: ctx.window_frame,
            out: ctx.out,
            text: ctx.text,
            text_truncated: ctx.text_truncated,
            busy: ctx.busy,
        };
        collect(child, &mut child_ctx);
    }
}

/// The container string children of `node` inherit, if `node` is one.
fn container_label(node: &RawNode, inherited: &Option<String>) -> Option<String> {
    const CONTAINER_ROLES: [&str; 11] = [
        "AXToolbar",
        "AXSheet",
        "AXDrawer",
        "AXTabGroup",
        "AXTable",
        "AXOutline",
        "AXList",
        "AXMenu",
        "AXMenuBar",
        "AXScrollArea",
        "AXGroup",
    ];
    if !CONTAINER_ROLES.contains(&node.role.as_str()) {
        return inherited.clone();
    }
    let role = display_role(&node.role, node.subrole.as_deref());
    let label = node.label();
    // An unlabelled generic group is not worth naming.
    if label.is_empty() && matches!(node.role.as_str(), "AXGroup" | "AXScrollArea") {
        return inherited.clone();
    }
    Some(if label.is_empty() {
        role
    } else {
        format!("{role} {label}")
    })
}

/// The text a node contributes to the observation's `text`.
///
/// Static text and headings give their label; a text area gives its content,
/// because in a document app that *is* the visible text and the 80-char
/// `value` of the element row cannot carry it.
fn visible_text(node: &RawNode) -> Option<String> {
    if node.is_secure() {
        return None;
    }
    if carries_text(node) {
        let label = node.label();
        return (!label.is_empty()).then_some(label);
    }
    if node.role == "AXTextArea" {
        let value = node.value.clone()?;
        return (!value.trim().is_empty()).then_some(value);
    }
    None
}

fn push_text(buf: &mut String, line: &str, truncated: &mut bool) {
    let line = collapse_whitespace(line);
    if line.is_empty() {
        return;
    }
    if *truncated {
        return;
    }
    let extra = if buf.is_empty() { 0 } else { 1 };
    let remaining = TEXT_BUDGET.saturating_sub(buf.chars().count() + extra);
    if remaining == 0 {
        *truncated = true;
        return;
    }
    if !buf.is_empty() {
        buf.push('\n');
    }
    if line.chars().count() > remaining {
        buf.extend(line.chars().take(remaining));
        *truncated = true;
    } else {
        buf.push_str(&line);
    }
}

/// The value shown next to an element, elided and redacted.
fn element_value(node: &RawNode) -> Option<String> {
    if node.is_secure() {
        return None;
    }
    // Check state is carried by `State::checked`, not by a raw "0"/"1".
    if matches!(
        node.role.as_str(),
        "AXCheckBox" | "AXRadioButton" | "AXMenuItem" | "AXDisclosureTriangle" | "AXStaticText"
    ) {
        return None;
    }
    let is_text_field = matches!(
        node.role.as_str(),
        "AXTextField" | "AXTextArea" | "AXSearchField" | "AXComboBox"
    );
    match node.value.as_deref().map(str::trim) {
        Some(v) if !v.is_empty() => Some(elide(&collapse_whitespace(v), VALUE_MAX)),
        _ if is_text_field => Some("empty".to_owned()),
        _ => None,
    }
}

/// Menu leaves ranked by token overlap with the goal, then menu order.
fn rank_menu<'a>(menu: &'a [MenuLeaf], goal: Option<&str>) -> Vec<&'a MenuLeaf> {
    let mut ranked: Vec<&MenuLeaf> = menu.iter().collect();
    let Some(goal) = goal else {
        return ranked;
    };
    let tokens: Vec<String> = goal
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(str::to_lowercase)
        .collect();
    if tokens.is_empty() {
        return ranked;
    }
    // Whole words, not substrings: "all" must not rank "Allocations & Leaks"
    // above "Select All".
    let score = |leaf: &MenuLeaf| -> usize {
        let words: Vec<String> = leaf
            .label()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        tokens
            .iter()
            .filter(|t| words.iter().any(|w| w == *t))
            .count()
    };
    // Stable sort: equal scores keep menu order.
    ranked.sort_by_key(|leaf| std::cmp::Reverse(score(leaf)));
    ranked
}

/// What the window's governing scroll area can still do.
///
/// Derived from the vertical scroll bar the walk already read: an enabled bar
/// whose `AXValue` (0..1) is off an end means there is content that way. A
/// missing or disabled bar means the content fits, so neither control is
/// offered.
pub(crate) fn scroll_affordance(root: &RawNode) -> ScrollAffordance {
    let mut best: Option<(f64, f64)> = None; // (height, value)
    root.walk(&mut |node| {
        if node.role != "AXScrollBar" || !node.enabled {
            return;
        }
        // Vertical bars are taller than they are wide.
        if node.frame.h <= node.frame.w {
            return;
        }
        let Some(value) = node
            .value
            .as_deref()
            .and_then(|v| v.trim().parse::<f64>().ok())
        else {
            return;
        };
        if best.is_none_or(|(h, _)| node.frame.h > h) {
            best = Some((node.frame.h, value));
        }
    });
    match best {
        Some((_, value)) => ScrollAffordance {
            up: value > 0.005,
            down: value < 0.995,
        },
        None => ScrollAffordance::default(),
    }
}

/// Keep the window budget diverse: no single kind of control may consume it.
///
/// Reading order alone is not enough on a large, repetitive surface. A
/// LibreOffice Calc sheet offers ~90 grid cells before anything else, and
/// measured on this machine the 250-row table contained **zero** non-cell text
/// inputs — so the sheet tabs, the toolbar and the formula controls were never
/// offered to the model at all.
///
/// The rule is app-agnostic: group by `(role, container)` and drop the tail of
/// any group that exceeds [`GROUP_SHARE`] of the budget, **keeping the
/// surviving elements in their original reading order**.
///
/// Order matters more than it looks. An earlier version of this took from the
/// groups round-robin, which is fairer but reorders the table: a single cell
/// changing value then moved most indices, every decision came back `STALE`,
/// and a run ended `Blocked("the surface changed under every decision")`
/// after five wasted Jev requests. Dropping a tail leaves every surviving
/// element where it was, so an index means the same thing across two
/// observations of an unchanged surface.
fn spread<'a>(ordered: Vec<Candidate<'a>>, cap: usize) -> Vec<Candidate<'a>> {
    if ordered.len() <= cap {
        return ordered;
    }
    let share = (cap * GROUP_SHARE_PERCENT / 100).max(1);
    let mut keys: Vec<(String, Option<String>)> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    // Pass one reserves each group's share, so a minority control can never be
    // crowded out. Pass two spends whatever is left in reading order, so a
    // surface that really is one big grid still fills the budget.
    let mut keep = vec![false; ordered.len()];
    for (position, candidate) in ordered.iter().enumerate() {
        let key = (candidate.node.role.clone(), candidate.container.clone());
        let slot = match keys.iter().position(|existing| *existing == key) {
            Some(slot) => slot,
            None => {
                keys.push(key);
                counts.push(0);
                counts.len() - 1
            }
        };
        if counts[slot] < share {
            counts[slot] += 1;
            keep[position] = true;
        }
    }
    // Pass two spends what is left. It goes round the groups rather than down
    // the list, because reading order hands the surplus straight back to the
    // group that already dominates: measured on Numbers, 209 menu items
    // refilled the budget and left **two** grid cells, even though pass one
    // had reserved room for the table. Selection order does not affect output
    // order — the result is emitted by original position below — so taking
    // round-robin here costs nothing in index stability.
    let mut taken = keep.iter().filter(|kept| **kept).count();
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); keys.len()];
    for (position, candidate) in ordered.iter().enumerate() {
        if keep[position] {
            continue;
        }
        let key = (candidate.node.role.clone(), candidate.container.clone());
        if let Some(slot) = keys.iter().position(|existing| *existing == key) {
            members[slot].push(position);
        }
    }
    let mut cursor = 0;
    while taken < cap && members.iter().any(|group| !group.is_empty()) {
        let slot = cursor % members.len().max(1);
        if let Some(position) = members.get_mut(slot).and_then(|group| {
            if group.is_empty() {
                None
            } else {
                Some(group.remove(0))
            }
        }) {
            keep[position] = true;
            taken += 1;
        }
        cursor += 1;
    }
    // Original order throughout: only a tail is dropped, so an index means the
    // same thing across two observations of an unchanged surface.
    ordered
        .into_iter()
        .zip(keep)
        .filter_map(|(candidate, kept)| kept.then_some(candidate))
        .take(cap)
        .collect()
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests opt out of unwrap_used (05 §2)")]
mod tests {
    use super::*;
    use crate::types::Operation;

    fn frame(x: f64, y: f64) -> Rect {
        Rect {
            x,
            y,
            w: 40.0,
            h: 20.0,
        }
    }

    fn app() -> AppInfo {
        AppInfo {
            name: "Notes".into(),
            bundle_id: Some("com.apple.Notes".into()),
            pid: 42,
            frontmost: true,
        }
    }

    fn window(children: Vec<RawNode>) -> RawNode {
        let mut w = RawNode::new(0, "AXWindow");
        w.title = Some("Untitled".into());
        w.frame = Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 800.0,
        };
        w.children = children;
        w
    }

    fn button(id: u32, title: &str, x: f64, y: f64) -> RawNode {
        let mut b = RawNode::new(id, "AXButton");
        b.title = Some(title.to_owned());
        b.frame = frame(x, y);
        b.actions = vec!["AXPress".into()];
        b
    }

    fn build(win: &RawNode, menu: &[MenuLeaf]) -> BuiltTable {
        build_table(TableInput {
            generation: 1,
            app: app(),
            window: win,
            menu,
            goal: None,
            scroll: ScrollAffordance::default(),
            settle_timed_out: false,
            url: None,
        })
    }

    #[test]
    fn a_modal_sheet_replaces_the_windows_elements() {
        let mut sheet = RawNode::new(100, "AXSheet");
        sheet.title = Some("Discard draft?".into());
        sheet.frame = Rect {
            x: 100.0,
            y: 100.0,
            w: 400.0,
            h: 200.0,
        };
        sheet.children = vec![button(101, "Don't Save", 120.0, 220.0)];

        let win = window(vec![button(1, "Send", 10.0, 10.0), sheet]);
        let built = build(&win, &[]);

        assert!(built.table.window.modal);
        let labels: Vec<&str> = built
            .table
            .elements
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(
            labels,
            vec!["Don't Save"],
            "nothing behind a modal is offered"
        );
        assert_eq!(built.table.window.title, "Untitled — Discard draft?");
        assert_eq!(
            built.table.elements[0].container.as_deref(),
            Some("sheet Discard draft?")
        );
    }

    #[test]
    fn elements_are_capped_at_250_with_menus_keeping_their_share() {
        let buttons: Vec<RawNode> = (0..400)
            .map(|i| button(i + 1, &format!("B{i}"), 10.0, f64::from(i)))
            .collect();
        let menu: Vec<MenuLeaf> = (0..300)
            .map(|i| MenuLeaf {
                id: 10_000 + i,
                path: vec!["File".into(), format!("M{i}")],
                shortcut: None,
                enabled: true,
            })
            .collect();

        let win = window(buttons);
        let built = build(&win, &menu);

        assert_eq!(built.table.elements.len(), MAX_ELEMENTS);
        assert!(built.table.truncated);
        let menus = built
            .table
            .elements
            .iter()
            .filter(|e| e.operations.contains(&Operation::Menu))
            .count();
        assert_eq!(menus, MENU_MIN, "menu leaves keep at least 80 slots");
        assert_eq!(built.table.elements.len() - menus, WINDOW_CAP);
        assert_eq!(built.raw_ids.len(), built.table.elements.len());
        assert_eq!(built.fingerprints.len(), built.table.elements.len());
    }

    #[test]
    /// A repetitive surface must not spend the whole budget on one kind of
    /// control. Measured on a real LibreOffice Calc sheet, the table held 250
    /// rows and **zero** non-cell text inputs, so the Name Box — the only
    /// control that moves the grid selection — was never offered to the model.
    fn one_repetitive_group_cannot_consume_the_whole_budget() {
        // A grid of 400 cells in one container, plus a handful of toolbar
        // controls that arrive after them in reading order.
        let mut nodes: Vec<RawNode> = (0..400)
            .map(|i| {
                let mut cell = button(i + 1, &format!("A{i}"), 10.0, f64::from(i));
                cell.role = "AXTextField".into();
                cell
            })
            .collect();
        for (offset, label) in ["Name Box", "Formula", "Sheet 1"].iter().enumerate() {
            let id = 9_000 + u32::try_from(offset).unwrap_or(0);
            let mut control = button(id, label, 400.0, 5.0);
            control.role = "AXTextArea".into();
            nodes.push(control);
        }
        let win = window(nodes);
        let built = build(&win, &[]);
        let labels: Vec<&str> = built
            .table
            .elements
            .iter()
            .map(|element| element.label.as_str())
            .collect();
        for wanted in ["Name Box", "Formula", "Sheet 1"] {
            assert!(
                labels.contains(&wanted),
                "`{wanted}` was crowded out by the grid"
            );
        }
        assert!(built.table.truncated);
    }

    #[test]
    fn window_elements_use_the_menu_slack_when_there_are_few_menus() {
        let buttons: Vec<RawNode> = (0..400)
            .map(|i| button(i + 1, &format!("B{i}"), 10.0, f64::from(i)))
            .collect();
        let menu: Vec<MenuLeaf> = (0..10)
            .map(|i| MenuLeaf {
                id: 10_000 + i,
                path: vec!["File".into(), format!("M{i}")],
                shortcut: None,
                enabled: true,
            })
            .collect();
        let win = window(buttons);
        let built = build(&win, &menu);
        assert_eq!(built.table.elements.len(), MAX_ELEMENTS);
        let menus = built
            .table
            .elements
            .iter()
            .filter(|e| e.operations.contains(&Operation::Menu))
            .count();
        assert_eq!(menus, 10);
    }

    #[test]
    fn a_small_window_is_not_truncated() {
        let win = window(vec![button(1, "Send", 10.0, 10.0)]);
        let built = build(&win, &[]);
        assert!(!built.table.truncated);
        assert_eq!(built.table.elements.len(), 1);
    }

    #[test]
    fn text_is_capped_at_6000_chars_and_sets_truncated() {
        let mut nodes = Vec::new();
        for i in 0..200 {
            let mut t = RawNode::new(i + 1, "AXStaticText");
            t.value = Some("x".repeat(100));
            t.frame = frame(10.0, f64::from(i));
            nodes.push(t);
        }
        let win = window(nodes);
        let built = build(&win, &[]);
        assert_eq!(built.table.text.chars().count(), TEXT_BUDGET);
        assert!(built.table.truncated);
    }

    #[test]
    fn text_is_visible_static_text_in_reading_order() {
        let mut a = RawNode::new(1, "AXStaticText");
        a.value = Some("First line".into());
        a.frame = frame(0.0, 0.0);
        let mut b = RawNode::new(2, "AXStaticText");
        b.value = Some("Second   line".into());
        b.frame = frame(0.0, 30.0);
        let win = window(vec![a, b]);
        let built = build(&win, &[]);
        assert_eq!(built.table.text, "First line\nSecond line");
        assert!(
            built.table.elements.is_empty(),
            "static text is text, not an action"
        );
    }

    #[test]
    fn values_are_elided_to_80_chars_and_blank_fields_read_empty() {
        let mut long = RawNode::new(1, "AXTextField");
        long.title = Some("Body".into());
        long.value = Some("y".repeat(200));
        long.settable_value = true;
        long.frame = frame(0.0, 0.0);

        let mut blank = RawNode::new(2, "AXTextField");
        blank.title = Some("To:".into());
        blank.value = Some("   ".into());
        blank.settable_value = true;
        blank.frame = frame(0.0, 30.0);

        let win = window(vec![long, blank]);
        let built = build(&win, &[]);
        let value = built.table.elements[0].value.as_deref().unwrap();
        assert_eq!(value.chars().count(), VALUE_MAX);
        assert!(value.ends_with('…'));
        assert_eq!(built.table.elements[1].value.as_deref(), Some("empty"));
    }

    #[test]
    fn a_secure_field_is_listed_with_no_value_and_no_operations() {
        let mut secure = RawNode::new(1, "AXTextField");
        secure.subrole = Some("AXSecureTextField".into());
        secure.title = Some("Password".into());
        secure.value = Some("hunter2".into());
        secure.settable_value = true;
        secure.frame = frame(0.0, 0.0);

        let win = window(vec![secure]);
        let built = build(&win, &[]);
        let e = &built.table.elements[0];
        assert_eq!(e.role, "securefield");
        assert_eq!(e.value, None);
        assert!(e.operations.is_empty());
        let json = serde_json::to_string(&built.table).unwrap();
        assert!(
            !json.contains("hunter2"),
            "a secure value never leaves the crate"
        );
    }

    #[test]
    fn on_screen_elements_come_before_off_screen_ones() {
        let visible = button(1, "Visible", 10.0, 10.0);
        let mut hidden = button(2, "Hidden", 10.0, 10.0);
        hidden.frame = Rect {
            x: 5000.0,
            y: 5000.0,
            w: 40.0,
            h: 20.0,
        };
        // Off-screen child first in tree order.
        let win = window(vec![hidden, visible]);
        let built = build(&win, &[]);
        let labels: Vec<&str> = built
            .table
            .elements
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Visible", "Hidden"]);
    }

    #[test]
    fn zero_size_nodes_are_dropped_but_their_children_are_kept() {
        let mut collapsed = RawNode::new(1, "AXGroup");
        collapsed.title = Some("Wrapper".into());
        collapsed.frame = Rect::default();
        collapsed.actions = vec!["AXPress".into()];
        collapsed.children = vec![button(2, "Inner", 10.0, 10.0)];
        let win = window(vec![collapsed]);
        let built = build(&win, &[]);
        let labels: Vec<&str> = built
            .table
            .elements
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Inner"]);
    }

    #[test]
    fn tunnelling_reaches_past_the_depth_budget_of_anonymous_groups() {
        let mut deep = button(999, "Deep", 10.0, 10.0);
        for _ in 0..60 {
            let mut wrapper = RawNode::new(0, "AXGroup");
            wrapper.frame = Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            };
            wrapper.children = vec![deep];
            deep = wrapper;
        }
        let win = window(vec![deep]);
        let built = build(&win, &[]);
        assert_eq!(
            built.table.elements.len(),
            1,
            "anonymous wrappers cost no depth"
        );
        assert_eq!(built.table.elements[0].label, "Deep");
    }

    #[test]
    fn real_depth_is_still_bounded() {
        let mut deep = button(999, "Deep", 10.0, 10.0);
        for i in 0..60 {
            let mut wrapper = RawNode::new(0, "AXGroup");
            wrapper.title = Some(format!("Named {i}"));
            wrapper.frame = Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            };
            wrapper.children = vec![deep];
            deep = wrapper;
        }
        let win = window(vec![deep]);
        let built = build(&win, &[]);
        assert!(
            !built.table.elements.iter().any(|e| e.label == "Deep"),
            "labelled nesting is real depth and must hit the budget"
        );
    }

    #[test]
    fn rows_are_numbered_in_the_container_label() {
        let mut table_node = RawNode::new(1, "AXTable");
        table_node.title = Some("Messages".into());
        table_node.frame = Rect {
            x: 0.0,
            y: 0.0,
            w: 500.0,
            h: 400.0,
        };
        for i in 0..3u32 {
            let mut row = RawNode::new(10 + i, "AXRow");
            row.frame = Rect {
                x: 0.0,
                y: f64::from(i) * 20.0,
                w: 500.0,
                h: 20.0,
            };
            row.children = vec![button(
                20 + i,
                &format!("Open {i}"),
                0.0,
                f64::from(i) * 20.0,
            )];
            table_node.children.push(row);
        }
        let win = window(vec![table_node]);
        let built = build(&win, &[]);
        let containers: Vec<Option<&str>> = built
            .table
            .elements
            .iter()
            .filter(|e| e.label.starts_with("Open"))
            .map(|e| e.container.as_deref())
            .collect();
        assert_eq!(
            containers,
            vec![Some("row 1"), Some("row 2"), Some("row 3")]
        );
    }

    #[test]
    fn controls_are_offered_only_when_valid() {
        let win = window(vec![button(1, "Send", 10.0, 10.0)]);
        let plain = build(&win, &[]);
        assert!(plain.table.controls.is_empty());

        let scrolled = build_table(TableInput {
            generation: 1,
            app: app(),
            window: &win,
            menu: &[],
            goal: None,
            scroll: ScrollAffordance {
                up: false,
                down: true,
            },
            settle_timed_out: true,
            url: None,
        });
        assert_eq!(
            scrolled.table.controls,
            vec![Control::ScrollDown, Control::Wait]
        );
    }

    #[test]
    fn scroll_controls_follow_the_vertical_scroll_bar() {
        let mut bar = RawNode::new(1, "AXScrollBar");
        bar.frame = Rect {
            x: 400.0,
            y: 0.0,
            w: 15.0,
            h: 300.0,
        };
        bar.value = Some("0".into());
        let win = window(vec![bar.clone()]);
        assert_eq!(
            scroll_affordance(&win),
            ScrollAffordance {
                up: false,
                down: true
            }
        );

        bar.value = Some("1".into());
        let win = window(vec![bar.clone()]);
        assert_eq!(
            scroll_affordance(&win),
            ScrollAffordance {
                up: true,
                down: false
            }
        );

        bar.value = Some("0.5".into());
        let win = window(vec![bar.clone()]);
        assert_eq!(
            scroll_affordance(&win),
            ScrollAffordance {
                up: true,
                down: true
            }
        );

        // A disabled bar means the content fits: neither control is offered.
        bar.enabled = false;
        let win = window(vec![bar.clone()]);
        assert_eq!(scroll_affordance(&win), ScrollAffordance::default());

        // A horizontal bar never drives SCROLL_UP / SCROLL_DOWN.
        let mut horizontal = RawNode::new(2, "AXScrollBar");
        horizontal.frame = Rect {
            x: 0.0,
            y: 300.0,
            w: 400.0,
            h: 15.0,
        };
        horizontal.value = Some("0.5".into());
        let win = window(vec![horizontal]);
        assert_eq!(scroll_affordance(&win), ScrollAffordance::default());

        // No scroll bar at all.
        assert_eq!(
            scroll_affordance(&window(vec![])),
            ScrollAffordance::default()
        );
    }

    #[test]
    fn a_busy_indicator_offers_wait() {
        let mut spinner = RawNode::new(1, "AXProgressIndicator");
        spinner.frame = frame(0.0, 0.0);
        let win = window(vec![spinner]);
        let built = build(&win, &[]);
        assert_eq!(built.table.controls, vec![Control::Wait]);
    }

    #[test]
    fn the_content_of_a_text_area_is_part_of_the_visible_text() {
        let mut area = RawNode::new(1, "AXTextArea");
        area.title = Some("Document".into());
        area.value = Some("the whole document body".into());
        area.settable_value = true;
        area.frame = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };

        let mut secret = RawNode::new(2, "AXTextArea");
        secret.subrole = Some("AXSecureTextField".into());
        secret.value = Some("hunter2".into());
        secret.frame = frame(0.0, 320.0);

        let win = window(vec![area, secret]);
        let built = build(&win, &[]);
        assert_eq!(built.table.text, "the whole document body");
        assert!(!built.table.text.contains("hunter2"));
    }

    #[test]
    fn goal_ranking_matches_whole_words_not_substrings() {
        let menu: Vec<MenuLeaf> = ["Allocations & Leaks", "Select All"]
            .iter()
            .enumerate()
            .map(|(i, name)| MenuLeaf {
                id: u32::try_from(i).unwrap(),
                path: vec!["Edit".into(), (*name).to_owned()],
                shortcut: None,
                enabled: true,
            })
            .collect();
        let win = window(vec![]);
        let built = build_table(TableInput {
            generation: 1,
            app: app(),
            window: &win,
            menu: &menu,
            goal: Some("select all the text"),
            scroll: ScrollAffordance::default(),
            settle_timed_out: false,
            url: None,
        });
        assert_eq!(built.table.elements[0].label, "Edit › Select All");
    }

    #[test]
    fn menu_leaves_are_ranked_by_goal_overlap_then_menu_order() {
        let menu: Vec<MenuLeaf> = ["New Note", "Export as PDF…", "Close"]
            .iter()
            .enumerate()
            .map(|(i, name)| MenuLeaf {
                id: u32::try_from(i).unwrap(),
                path: vec!["File".into(), (*name).to_owned()],
                shortcut: None,
                enabled: true,
            })
            .collect();
        let win = window(vec![]);
        let built = build_table(TableInput {
            generation: 1,
            app: app(),
            window: &win,
            menu: &menu,
            goal: Some("export the note as a pdf"),
            scroll: ScrollAffordance::default(),
            settle_timed_out: false,
            url: None,
        });
        assert_eq!(built.table.elements[0].label, "File › Export as PDF…");
        assert_eq!(built.table.elements[1].label, "File › New Note");
    }

    #[test]
    fn a_disabled_menu_leaf_has_no_operations() {
        let menu = vec![MenuLeaf {
            id: 1,
            path: vec!["Edit".into(), "Undo".into()],
            shortcut: Some("⌘Z".into()),
            enabled: false,
        }];
        let win = window(vec![]);
        let built = build(&win, &menu);
        assert!(built.table.elements[0].operations.is_empty());
        assert_eq!(built.table.elements[0].value.as_deref(), Some("⌘Z"));
    }

    #[test]
    fn serialised_table_never_leaks_a_ref_or_a_frame() {
        let mut secure = RawNode::new(2, "AXSecureTextField");
        secure.title = Some("Password".into());
        secure.frame = frame(0.0, 40.0);
        let win = window(vec![button(1, "Send", 10.0, 10.0), secure]);
        let built = build(&win, &[]);

        let json = serde_json::to_value(&built.table).unwrap();
        let mut stack = vec![json];
        while let Some(value) = stack.pop() {
            match value {
                serde_json::Value::Object(map) => {
                    for (k, v) in map {
                        assert_ne!(k, "r", "a ref must never be serialised");
                        assert_ne!(k, "frame", "a frame must never be serialised");
                        stack.push(v);
                    }
                }
                serde_json::Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
    }

    #[test]
    fn table_json_has_the_shape_the_policy_consumes() {
        let win = window(vec![button(1, "Send", 10.0, 10.0)]);
        let built = build(&win, &[]);
        let json = serde_json::to_value(&built.table).unwrap();
        for key in [
            "generation",
            "app",
            "window",
            "text",
            "elements",
            "controls",
            "truncated",
        ] {
            assert!(json.get(key).is_some(), "missing key {key}");
        }
        let element = &json["elements"][0];
        assert_eq!(element["index"], 0);
        assert_eq!(element["role"], "button");
        assert_eq!(element["label"], "Send");
        assert_eq!(element["operations"][0], "CLICK");
        assert_eq!(element["state"]["enabled"], true);
    }
}
