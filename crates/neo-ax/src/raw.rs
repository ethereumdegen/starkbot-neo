//! The intermediate tree the AX walk produces and the pure layers consume.
//!
//! `RawNode` is the only thing `table.rs` and `mapping.rs` ever see, which is
//! what makes pruning, budgeting and the operation mapping testable without a
//! Mac UI: a test builds `RawNode`s by hand, the live walk builds them from
//! `AXUIElementCopyMultipleAttributeValues`.
//!
//! `id` is a dense index into the actor's element store. Nothing outside the
//! actor thread ever resolves it.
//!
//! Off macOS only the walker that fills these nodes is gated away
//! (`cfg(target_os = "macos")`), so the type and its text helpers look unused
//! there. They stay compiled with `mapping.rs` and `table.rs`, whose tests
//! build `RawNode`s by hand: that spec is worth running on the Linux lane,
//! and the lane denies warnings.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::types::Rect;

/// One node of the unpruned AX tree, with every attribute the bulk fetch asks
/// for already decoded.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RawNode {
    /// Dense index into the actor's element store for this walk.
    pub id: u32,
    /// `AXRole`, e.g. `AXButton`.
    pub role: String,
    /// `AXSubrole`, e.g. `AXSecureTextField`.
    pub subrole: Option<String>,
    /// `AXTitle`.
    pub title: Option<String>,
    /// `AXDescription`.
    pub description: Option<String>,
    /// `AXValue`, already dropped for secure fields by the walker.
    pub value: Option<String>,
    /// `AXPlaceholderValue`.
    pub placeholder: Option<String>,
    /// `AXHelp`.
    pub help: Option<String>,
    /// `AXIdentifier`.
    pub identifier: Option<String>,
    /// `AXURL`, on web areas and links.
    pub url: Option<String>,
    /// `AXEnabled`, defaulting to true when the app does not say.
    pub enabled: bool,
    /// `AXFocused`.
    pub focused: bool,
    /// `AXSelected`.
    pub selected: bool,
    /// `AXExpanded`, absent when the role has no disclosure state.
    pub expanded: Option<bool>,
    /// Position and size in global points.
    pub frame: Rect,
    /// Action names from `AXUIElementCopyActionNames`.
    pub actions: Vec<String>,
    /// Whether `AXValue` is settable.
    pub settable_value: bool,
    /// Whether the backend can put the caret in this node itself, even though its value is
    /// not settable. AT-SPI's `Component.GrabFocus` plus the virtual keyboard is such a path,
    /// and WebKitGTK needs it: it implements no `EditableText`, so every text field in a
    /// Tauri window reports `settable_value == false` and only the one the page happened to
    /// autofocus would otherwise be typeable — the rest would carry no operation at all and
    /// be unreachable. Always false on macOS, where a settable `AXValue` is the path.
    pub focusable_text: bool,
    /// Whether the node is one of a set the user picks from — an AT-SPI
    /// `Selectable` row. Its sibling above exists for the same reason: a
    /// WebKitGTK listbox option implements no `Action` and is not a click
    /// role, so without this the rows of an open combobox popup carry no
    /// operation at all and the only reachable element is the input that
    /// opened it. Measured on degen-paint Studio's command palette, where
    /// the op the navigator had correctly filtered down to could be read and
    /// not chosen. Always false on macOS, where such a row advertises
    /// `AXPress`.
    pub selectable: bool,
    /// Enumerable choices, when the control exposes them while closed.
    pub options: Vec<String>,
    /// Children in reading order, already tunnelled through by the walker only
    /// in the sense that they are the raw `AXChildren`.
    pub children: Vec<RawNode>,
}

impl RawNode {
    /// A node with only a role, for tests and for placeholder roots.
    #[cfg(test)]
    pub(crate) fn new(id: u32, role: &str) -> Self {
        Self {
            id,
            role: role.to_owned(),
            enabled: true,
            ..Self::default()
        }
    }

    /// Whether this node is a secure text field. Its value is never exposed.
    pub(crate) fn is_secure(&self) -> bool {
        self.role == "AXSecureTextField" || self.subrole.as_deref() == Some("AXSecureTextField")
    }

    /// Label precedence: title, description, placeholder, help, then value for
    /// roles whose value *is* their label (static text, menu items).
    pub(crate) fn label(&self) -> String {
        let pick = [
            self.title.as_deref(),
            self.description.as_deref(),
            self.placeholder.as_deref(),
            self.help.as_deref(),
        ];
        for candidate in pick.into_iter().flatten() {
            let trimmed = candidate.trim();
            if !trimmed.is_empty() {
                return collapse_whitespace(trimmed);
            }
        }
        if self.role == "AXStaticText"
            && !self.is_secure()
            && let Some(v) = self.value.as_deref()
        {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return collapse_whitespace(trimmed);
            }
        }
        String::new()
    }

    /// Depth-first iteration over the node and its descendants.
    pub(crate) fn walk(&self, visit: &mut impl FnMut(&RawNode)) {
        visit(self);
        for child in &self.children {
            child.walk(visit);
        }
    }
}

/// One leaf of the menu bar, read without opening any menu.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MenuLeaf {
    /// Dense index into the actor's element store.
    pub id: u32,
    /// Full path from the menu-bar item down, e.g. `["File", "Export", "PDF…"]`.
    pub path: Vec<String>,
    /// Keyboard shortcut rendered for display, e.g. `⌘S`.
    pub shortcut: Option<String>,
    /// Whether the item is enabled right now.
    pub enabled: bool,
}

impl MenuLeaf {
    /// The label Jev sees: the full path joined with `›`.
    pub(crate) fn label(&self) -> String {
        self.path.join(MENU_SEPARATOR)
    }
}

/// What joins a menu leaf's path segments into the one label a flat table can
/// offer.
pub(crate) const MENU_SEPARATOR: &str = " › ";

/// Is `live` — the label an element reports about itself — the same element
/// the table recorded as `observed`?
///
/// Menu rows are the one place the two legitimately differ. The table is
/// flat, so a menu leaf is written as its whole path (`File › New Project…`)
/// to be nameable at all; the node itself only ever reports its own segment
/// (`New Project…`). A guard comparing the two literally found *every* menu
/// item stale on every read, which is not a near-miss: five consecutive stale
/// reads block the run, so no menu item could be actioned at all — and menus
/// are how an application is asked to import, export or start a document.
/// Observed on degen-paint Studio, where `File › New Project…` was chosen
/// with p=0.97 five times and executed zero times.
///
/// Only the trailing segment is allowed to stand in for the path. Anything
/// looser would let a guard accept a *different* menu item with the same leaf
/// name — `File › Close` approving a click on `Edit › Close`.
pub(crate) fn label_matches(observed: &str, live: &str) -> bool {
    observed == live
        || observed
            .rsplit_once(MENU_SEPARATOR)
            .is_some_and(|(_, leaf)| leaf == live)
}

/// Render a menu item's keyboard shortcut from `AXMenuItemCmdChar` and
/// `AXMenuItemCmdModifiers`.
///
/// The modifier mask is Carbon's: bit 0 is shift, bit 1 is option, bit 2 is
/// control, and bit 3 *suppresses* the otherwise implicit command key.
// Carbon's mask and the ⌘ glyphs are a macOS shape; an AT-SPI backend reads
// accelerators as ready-made strings and never calls this. The unit test
// below still exercises it everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn render_shortcut(cmd_char: Option<&str>, modifiers: u32) -> Option<String> {
    let key = cmd_char?.trim();
    if key.is_empty() {
        return None;
    }
    let mut out = String::new();
    if modifiers & 0b100 != 0 {
        out.push('⌃');
    }
    if modifiers & 0b010 != 0 {
        out.push('⌥');
    }
    if modifiers & 0b001 != 0 {
        out.push('⇧');
    }
    if modifiers & 0b1000 == 0 {
        out.push('⌘');
    }
    out.push_str(&key.to_uppercase());
    Some(out)
}

/// Fold runs of whitespace (including newlines) into single spaces.
pub(crate) fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = false;
            out.push(ch);
        }
    }
    out
}

/// Truncate to `max` characters, appending `…` when anything was cut.
pub(crate) fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_precedence_is_title_then_description_then_placeholder_then_help() {
        let mut n = RawNode::new(0, "AXTextField");
        n.help = Some("Help".into());
        assert_eq!(n.label(), "Help");
        n.placeholder = Some("Search".into());
        assert_eq!(n.label(), "Search");
        n.description = Some("Search field".into());
        assert_eq!(n.label(), "Search field");
        n.title = Some("Query".into());
        assert_eq!(n.label(), "Query");
    }

    #[test]
    fn static_text_falls_back_to_its_value_but_a_secure_field_never_does() {
        let mut text = RawNode::new(0, "AXStaticText");
        text.value = Some("Dana Ruiz —  Q3\nnumbers".into());
        assert_eq!(text.label(), "Dana Ruiz — Q3 numbers");

        let mut secure = RawNode::new(1, "AXStaticText");
        secure.subrole = Some("AXSecureTextField".into());
        secure.value = Some("hunter2".into());
        assert_eq!(secure.label(), "");
    }

    #[test]
    fn blank_labels_are_empty_not_whitespace() {
        let mut n = RawNode::new(0, "AXButton");
        n.title = Some("   ".into());
        n.description = Some("\n\t".into());
        assert_eq!(n.label(), "");
    }

    #[test]
    fn elide_keeps_char_boundaries_and_marks_the_cut() {
        assert_eq!(elide("abc", 5), "abc");
        assert_eq!(elide("abcdef", 4), "abc…");
        assert_eq!(elide("héllo wörld", 4), "hél…");
        assert_eq!(elide("🙂🙂🙂🙂", 2), "🙂…");
    }

    #[test]
    fn menu_leaf_label_is_the_full_path() {
        let leaf = MenuLeaf {
            id: 0,
            path: vec!["File".into(), "Export".into(), "PDF…".into()],
            shortcut: Some("⌘S".into()),
            enabled: true,
        };
        assert_eq!(leaf.label(), "File › Export › PDF…");
    }

    #[test]
    fn shortcuts_render_in_carbon_modifier_order() {
        assert_eq!(render_shortcut(Some("s"), 0).as_deref(), Some("⌘S"));
        assert_eq!(render_shortcut(Some("s"), 0b0001).as_deref(), Some("⇧⌘S"));
        assert_eq!(render_shortcut(Some("s"), 0b0111).as_deref(), Some("⌃⌥⇧⌘S"));
        // Bit 3 suppresses the implicit command key.
        assert_eq!(render_shortcut(Some("F2"), 0b1000).as_deref(), Some("F2"));
        assert_eq!(render_shortcut(None, 0), None);
        assert_eq!(render_shortcut(Some(" "), 0), None);
    }

    /// The defect this guards: a menu row is recorded as its whole path but
    /// reports only its leaf, so a literal comparison made every menu item
    /// permanently stale and nothing on a menu could be clicked.
    #[test]
    fn a_menu_row_matches_the_leaf_the_element_reports() {
        assert!(label_matches("File › New Project…", "New Project…"));
        assert!(label_matches("View › Panes › Layers", "Layers"));
        // A plain element is still compared whole.
        assert!(label_matches("New project", "New project"));
    }

    /// The identity check must not be loosened into "any item with this
    /// name": two menus commonly share a leaf, and approving one must never
    /// authorise a click on the other.
    #[test]
    fn a_leaf_name_alone_does_not_match_a_different_menu() {
        assert!(!label_matches("File › Close", "Edit › Close"));
        assert!(!label_matches("File › New Project…", "Open Project…"));
        assert!(!label_matches("Send", "Sending"));
    }
}
