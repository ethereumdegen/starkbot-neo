//! The `AX role → operation` mapping of `plans/01-accessibility.md`.
//!
//! Pure: it reads a `RawNode` and answers what Jev may do with it. No AX call
//! happens here, so every row of the table in the plan is unit-testable.
//!
//! # One policy, two vocabularies
//!
//! 17 §3.1 asks for "the same sets in `atspi::Role` terms; one table, not
//! two policies", and this is how that is kept true. The sets below —
//! `CLICK_ROLES`, `TEXT_ROLES`, `SELECT_ROLES`, `DECORATION_ROLES`,
//! `TUNNEL_ROLES`, `MODAL_ROLES`, `BUSY_ROLES` — are the policy, and they
//! are keyed on one *canonical* role vocabulary: the `AX…` names, because
//! that is the vocabulary Jev, the packs, the fixtures and
//! `plans/01-accessibility.md` already speak. The macOS backend produces
//! those names natively. The AT-SPI backend translates into them once, in
//! [`canonical_role`] below, which sits here rather than in
//! `backend::atspi` precisely so that a role added to a policy set and a
//! role arriving from a toolkit cannot drift into different files.
//!
//! Nothing downstream of this module knows which backend produced a
//! `RawNode`.

use crate::raw::RawNode;
use crate::types::{Checked, Operation, State};

/// `AXPress` and friends, in the order `press` tries them.
pub(crate) const PRESS_ACTIONS: [&str; 4] = ["AXPress", "AXConfirm", "AXPick", "AXShowMenu"];

/// Roles that are clickable by nature, with or without an advertised action.
const CLICK_ROLES: [&str; 12] = [
    "AXButton",
    "AXLink",
    "AXCheckBox",
    "AXRadioButton",
    "AXMenuButton",
    "AXPopUpButton",
    "AXDisclosureTriangle",
    "AXMenuItem",
    "AXMenuBarItem",
    "AXToolbarButton",
    "AXIncrementor",
    "AXColorWell",
];

/// Roles that take typed text.
const TEXT_ROLES: [&str; 4] = ["AXTextField", "AXTextArea", "AXSearchField", "AXComboBox"];

/// Roles whose options can be enumerated without opening anything.
const SELECT_ROLES: [&str; 7] = [
    "AXRadioGroup",
    "AXTabGroup",
    "AXSegmentedControl",
    "AXComboBox",
    "AXPopUpButton",
    "AXRow",
    "AXOutlineRow",
];

/// Roles that never reach the element table: pure chrome.
const DECORATION_ROLES: [&str; 6] = [
    "AXScrollBar",
    "AXSplitter",
    "AXGrowArea",
    "AXValueIndicator",
    "AXRulerMarker",
    "AXRuler",
];

/// Anonymous wrappers that cost no depth and are never emitted.
const TUNNEL_ROLES: [&str; 6] = [
    "AXGroup",
    "AXGenericElement",
    "AXSplitGroup",
    "AXLayoutArea",
    "AXLayoutItem",
    "AXUnknown",
];

/// Roles that mark the observation as modal and replace the window's elements.
const MODAL_ROLES: [&str; 2] = ["AXSheet", "AXDrawer"];

/// Subroles that make a plain window a modal dialog.
const MODAL_SUBROLES: [&str; 3] = ["AXDialog", "AXSystemDialog", "AXSystemFloatingWindow"];

/// Roles that mean "the app is busy".
const BUSY_ROLES: [&str; 2] = ["AXProgressIndicator", "AXBusyIndicator"];

/// One AT-SPI action name in the `AX…` vocabulary the policy sets are keyed
/// on, or the name unchanged when it means nothing here.
///
/// Roles were canonicalised from the start and actions were not, which left
/// half a translation: [`operations`] asks whether a node advertises one of
/// [`PRESS_ACTIONS`] — all of them `AX…` names — while AT-SPI answers
/// `GetName` with ATK's own vocabulary (`click`, `press`, `activate`, …). The
/// test never matched on Linux, so the only elements that could be clicked
/// were the ones a *role* made clickable, and anything that is activatable
/// only by its action was inert.
///
/// Measured on degen-paint Studio: the command palette's filtered options
/// arrive as `ListItem` → `AXRow`, which is not a click role, and carried
/// `click` as their action. They were shown to Jev with an empty operation
/// list — visible, correctly ranked, and impossible to choose. Every list
/// option, every result row and every custom activatable widget on the
/// toolkit was in that state.
#[cfg(target_os = "linux")]
pub(crate) fn canonical_action(name: &str) -> &str {
    // ATK has no fixed enum here: the names are conventions, lower-cased and
    // occasionally spaced. Compared case-insensitively without allocating by
    // matching the shapes actually published.
    match name.trim().to_ascii_lowercase().as_str() {
        // "jump" is what ATK calls following a link; "open" is what a file
        // manager row publishes. Both are "activate this" to a navigator.
        "click" | "press" | "activate" | "jump" | "open" | "default" => "AXPress",
        "showmenu" | "show menu" | "show-menu" | "menu" => "AXShowMenu",
        "pick" | "select" => "AXPick",
        "confirm" => "AXConfirm",
        _ => name,
    }
}

/// The AT-SPI2 role vocabulary, said in the canonical one.
///
/// Returns `(role, subrole)` in `AX…` terms, which is the only vocabulary
/// the sets above and [`display_role`] know. `states` is consulted where
/// AT-SPI splits with a state what macOS splits with a role: a single-line
/// `Text` is a field, a multi-line one is an area.
///
/// Measured deviations from what a reading of the two APIs would suggest,
/// each of which cost a run on this machine:
///
/// * AT-SPI has no *subrole*, so `Dialog` and `Alert` become
///   `AXWindow`/`AXDialog` — the subrole `MODAL_SUBROLES` already knows.
///   `State::Modal` is **not** required: Chromium sets it on a `<dialog>`,
///   WebKitGTK does not set it on `role="dialog" aria-modal="true"`, and a
///   dialog that is not treated as modal leaves Jev acting on the window
///   behind it.
/// * `ToggleButton` is `AXCheckBox` with the `AXToggle` subrole rather
///   than a button, so its on/off state reaches `State::checked` instead of
///   being invisible.
/// * `PageTab` is `AXRadioButton`/`AXTabButton`, which is exactly the shape
///   AppKit publishes for a tab and what `display_role` renders as `tab`.
/// * `ListItem` is `AXRow`: it is what a web `role="option"` arrives as
///   from both Chromium and WebKitGTK, and a row is selectable.
#[cfg(target_os = "linux")]
pub(crate) fn canonical_role(
    role: atspi::Role,
    states: atspi::StateSet,
) -> (&'static str, Option<&'static str>) {
    use atspi::{Role, State};

    let multi_line = states.contains(State::MultiLine);
    match role {
        // Clickable.
        Role::Button | Role::PushButtonMenu => ("AXButton", None),
        Role::ToggleButton => ("AXCheckBox", Some("AXToggle")),
        Role::CheckBox => ("AXCheckBox", None),
        Role::RadioButton => ("AXRadioButton", None),
        Role::Link => ("AXLink", None),
        Role::MenuItem | Role::CheckMenuItem | Role::RadioMenuItem | Role::TearoffMenuItem => {
            ("AXMenuItem", None)
        }
        Role::PageTab => ("AXRadioButton", Some("AXTabButton")),
        Role::SpinButton => ("AXIncrementor", None),
        Role::Slider | Role::Dial => ("AXSlider", None),
        Role::ColorChooser => ("AXColorWell", None),

        // Typed into.
        Role::PasswordText => ("AXSecureTextField", None),
        Role::Entry | Role::Text | Role::Editbar | Role::DateEditor => {
            if multi_line {
                ("AXTextArea", None)
            } else {
                ("AXTextField", None)
            }
        }
        Role::ComboBox | Role::Autocomplete => ("AXComboBox", None),

        // Chosen from.
        Role::PageTabList => ("AXTabGroup", None),
        Role::ListItem | Role::TableRow => ("AXRow", None),
        Role::TreeItem => ("AXRow", Some("AXOutlineRow")),
        Role::List | Role::ListBox => ("AXList", None),
        Role::Table => ("AXTable", None),
        Role::Tree | Role::TreeTable => ("AXOutline", None),
        Role::TableCell => ("AXCell", None),
        Role::ColumnHeader | Role::TableColumnHeader => ("AXColumnHeader", None),
        Role::RowHeader | Role::TableRowHeader => ("AXRowHeader", None),

        // Structure that carries a name worth showing as a container.
        Role::MenuBar => ("AXMenuBar", None),
        Role::Menu | Role::PopupMenu => ("AXMenu", None),
        Role::ToolBar => ("AXToolbar", None),
        Role::ScrollPane | Role::Viewport => ("AXScrollArea", None),
        Role::Frame | Role::Window | Role::InternalFrame | Role::DesktopFrame => ("AXWindow", None),
        Role::Dialog | Role::Alert => ("AXWindow", Some("AXDialog")),
        Role::Application => ("AXApplication", None),
        Role::DocumentWeb => ("AXWebArea", None),

        // Text that is read, not driven.
        Role::Label | Role::Static | Role::Paragraph | Role::Caption | Role::BlockQuote => {
            ("AXStaticText", None)
        }
        Role::Heading => ("AXHeading", None),
        Role::Image | Role::Icon | Role::Canvas | Role::DrawingArea => ("AXImage", None),

        // Busy.
        Role::ProgressBar | Role::LevelBar => ("AXProgressIndicator", None),

        // Pure chrome.
        Role::ScrollBar => ("AXScrollBar", None),
        Role::Separator => ("AXSplitter", None),
        Role::Ruler => ("AXRuler", None),

        // Anonymous wrappers. `AXGroup` and friends tunnel when they carry
        // no label and no action, which is what these overwhelmingly are:
        // a GTK window is a dozen nested `Filler`s before the first widget.
        Role::SplitPane => ("AXSplitGroup", None),
        Role::Panel
        | Role::Filler
        | Role::Grouping
        | Role::Section
        | Role::Landmark
        | Role::Form
        | Role::RootPane
        | Role::LayeredPane
        | Role::OptionPane
        | Role::GlassPane
        | Role::Header
        | Role::Footer
        | Role::Article
        | Role::StatusBar
        | Role::InfoBar
        | Role::Notification
        | Role::ToolTip
        | Role::HTMLContainer
        | Role::DocumentFrame
        | Role::DocumentText
        | Role::DocumentSpreadsheet
        | Role::DocumentPresentation
        | Role::DocumentEmail
        | Role::Terminal => ("AXGroup", None),

        _ => ("AXUnknown", None),
    }
}

/// Lower-cased, de-`AX`-ed role as Jev sees it.
pub(crate) fn display_role(role: &str, subrole: Option<&str>) -> String {
    match subrole {
        Some("AXSecureTextField") => return "securefield".to_owned(),
        Some("AXSearchField") => return "searchfield".to_owned(),
        Some("AXTabButton") => return "tab".to_owned(),
        Some("AXToggle") => return "toggle".to_owned(),
        Some("AXDialog" | "AXSystemDialog") => return "dialog".to_owned(),
        Some("AXOutlineRow") => return "row".to_owned(),
        _ => {}
    }
    match role {
        "AXSecureTextField" => "securefield".to_owned(),
        "AXStaticText" => "text".to_owned(),
        "AXMenuBarItem" => "menubaritem".to_owned(),
        other => other.strip_prefix("AX").unwrap_or(other).to_lowercase(),
    }
}

/// Whether the node is a decoration that never reaches the table.
pub(crate) fn is_decoration(node: &RawNode) -> bool {
    DECORATION_ROLES.contains(&node.role.as_str())
}

/// Whether the node is an anonymous wrapper that costs no depth.
pub(crate) fn is_tunnel(node: &RawNode) -> bool {
    TUNNEL_ROLES.contains(&node.role.as_str())
        && node.label().is_empty()
        && !node
            .actions
            .iter()
            .any(|a| PRESS_ACTIONS.contains(&a.as_str()))
}

/// Whether the node is a sheet or modal dialog.
pub(crate) fn is_modal(node: &RawNode) -> bool {
    MODAL_ROLES.contains(&node.role.as_str())
        || node
            .subrole
            .as_deref()
            .is_some_and(|s| MODAL_SUBROLES.contains(&s))
}

/// Whether the node says the app is working on something.
pub(crate) fn is_busy(node: &RawNode) -> bool {
    BUSY_ROLES.contains(&node.role.as_str())
}

/// Whether the node is worth a row in the element table, action or not.
///
/// Disabled controls stay: Jev needs to see that `Send` exists and is greyed
/// out rather than conclude the button is missing. Secure fields stay for the
/// same reason, with no operations.
pub(crate) fn is_table_worthy(node: &RawNode) -> bool {
    if is_decoration(node) {
        return false;
    }
    if node.is_secure() {
        return true;
    }
    CLICK_ROLES.contains(&node.role.as_str())
        || TEXT_ROLES.contains(&node.role.as_str())
        || SELECT_ROLES.contains(&node.role.as_str())
        || node
            .actions
            .iter()
            .any(|a| PRESS_ACTIONS.contains(&a.as_str()))
        || (node.settable_value && !node.role.is_empty())
}

/// Whether the node contributes to the observation's `text`.
pub(crate) fn carries_text(node: &RawNode) -> bool {
    !node.is_secure() && matches!(node.role.as_str(), "AXStaticText" | "AXHeading")
}

/// The `AX → operation` mapping of the plan, applied to one node.
///
/// `is_menu_leaf` is set by the caller for enabled leaf menu-bar items, which
/// are the only source of `MENU`.
pub(crate) fn operations(node: &RawNode, is_menu_leaf: bool) -> Vec<Operation> {
    // A secure field is shown but is never operable: Jev must answer BLOCKED.
    if node.is_secure() {
        return Vec::new();
    }
    if !node.enabled {
        return Vec::new();
    }
    if is_menu_leaf {
        return vec![Operation::Menu];
    }

    let mut ops = Vec::new();
    let role = node.role.as_str();
    let has_press = node
        .actions
        .iter()
        .any(|a| PRESS_ACTIONS.contains(&a.as_str()));

    if CLICK_ROLES.contains(&role) || has_press {
        ops.push(Operation::Click);
    }

    let editable_web = role == "AXWebArea" && node.settable_value;
    let typeable = node.settable_value || node.focused || node.focusable_text;
    if (TEXT_ROLES.contains(&role) || editable_web) && typeable {
        ops.push(Operation::TypeText);
    }

    // A text field the backend can focus is also worth a CLICK even when it advertises no
    // action: clicking is how a navigator puts the caret somewhere before typing, and a row
    // with no operation at all is a row it cannot reach.
    if TEXT_ROLES.contains(&role) && node.focusable_text && !ops.contains(&Operation::Click) {
        ops.push(Operation::Click);
    }

    // A row the user is meant to pick from is a row a navigator must be able
    // to pick. `Selectable` is the honest signal for that and is the only
    // one used here.
    //
    // Offering CLICK on *any* labelled row was tried and reverted. A
    // WebKitGTK `role="option"` sets neither `Selectable` nor `Action`, so
    // the rule had to fall back to "row with a label", and the synthetic
    // click it promised does not activate such a row — the app binds
    // activation to a handler the pointer event does not reach. The effect
    // was worse than the gap: Jev spent its steps clicking a command
    // palette's option instead of pressing Enter, which is the path that
    // does choose it and which the app documents.
    if node.selectable && !ops.contains(&Operation::Click) {
        ops.push(Operation::Click);
    }

    // SELECT only when the choices are already known: a pop-up button that
    // hides its menu until it is opened stays a CLICK target.
    if SELECT_ROLES.contains(&role) && !node.options.is_empty() {
        ops.push(Operation::Select);
    }

    // …and a chooser that is *open* stops being a CLICK target, which is the
    // same rule read the other way. Clicking a chooser is how it is opened;
    // once it is open, a click closes it or does nothing, and it is
    // indistinguishable to a classifier from the action that would make
    // progress. Gated on `expanded` rather than on having options, because a
    // closed AppKit pop-up publishes its menu items too and must stay
    // clickable. Measured on degen-paint Studio: with the command palette
    // open and filtered to the one op the goal named, five steps in a row
    // chose CLICK on the palette at p≈0.2 and nothing happened, instead of
    // the SELECT sitting beside it.
    if node.expanded == Some(true) && ops.contains(&Operation::Select) {
        ops.retain(|op| *op != Operation::Click);
    }

    ops
}

/// The bare state words for a node.
pub(crate) fn state_of(node: &RawNode) -> State {
    let checked = match node.role.as_str() {
        "AXCheckBox" | "AXRadioButton" | "AXMenuItem" | "AXDisclosureTriangle" => {
            node.value.as_deref().and_then(parse_checked)
        }
        _ => None,
    };
    State {
        enabled: node.enabled,
        focused: node.focused,
        selected: node.selected,
        expanded: node.expanded.unwrap_or(false),
        checked,
    }
}

fn parse_checked(value: &str) -> Option<Checked> {
    match value.trim() {
        "1" | "true" => Some(Checked::On),
        "0" | "false" => Some(Checked::Off),
        "2" | "mixed" => Some(Checked::Mixed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: &str) -> RawNode {
        RawNode::new(0, role)
    }

    #[test]
    fn click_is_offered_on_the_roles_the_plan_lists() {
        for role in [
            "AXButton",
            "AXLink",
            "AXCheckBox",
            "AXRadioButton",
            "AXMenuButton",
            "AXPopUpButton",
            "AXDisclosureTriangle",
            "AXMenuItem",
        ] {
            assert!(
                operations(&node(role), false).contains(&Operation::Click),
                "{role} must offer CLICK"
            );
        }
    }

    #[test]
    fn click_is_offered_on_a_cell_row_or_image_only_when_it_advertises_press() {
        for role in ["AXCell", "AXRow", "AXImage"] {
            let plain = node(role);
            assert!(
                !operations(&plain, false).contains(&Operation::Click),
                "{role} without AXPress must not offer CLICK"
            );
            let mut pressable = node(role);
            pressable.actions = vec!["AXPress".into()];
            assert!(
                operations(&pressable, false).contains(&Operation::Click),
                "{role} with AXPress must offer CLICK"
            );
        }
    }

    #[test]
    fn type_text_needs_a_settable_value_or_focus_and_never_a_secure_field() {
        let mut field = node("AXTextField");
        assert!(!operations(&field, false).contains(&Operation::TypeText));
        field.settable_value = true;
        assert!(operations(&field, false).contains(&Operation::TypeText));

        let mut focused = node("AXTextArea");
        focused.focused = true;
        assert!(operations(&focused, false).contains(&Operation::TypeText));

        let mut secure = node("AXTextField");
        secure.subrole = Some("AXSecureTextField".into());
        secure.settable_value = true;
        secure.focused = true;
        assert!(
            operations(&secure, false).is_empty(),
            "a secure field is never operable"
        );
    }

    #[test]
    fn editable_web_area_text_is_typable() {
        let mut web = node("AXWebArea");
        web.settable_value = true;
        assert!(operations(&web, false).contains(&Operation::TypeText));
    }

    #[test]
    fn select_needs_options_that_are_known_while_closed() {
        let mut popup = node("AXPopUpButton");
        assert_eq!(
            operations(&popup, false),
            vec![Operation::Click],
            "closed pop-up stays CLICK"
        );
        popup.options = vec!["A".into(), "B".into()];
        assert_eq!(
            operations(&popup, false),
            vec![Operation::Click, Operation::Select]
        );

        let mut group = node("AXRadioGroup");
        group.options = vec!["One".into()];
        assert_eq!(operations(&group, false), vec![Operation::Select]);
    }

    /// A chooser that is already open must not still offer the click that
    /// opens it: with both on the table the classifier picked CLICK over and
    /// over and the run made no progress.
    #[test]
    fn an_open_chooser_offers_the_choice_and_not_the_click() {
        let mut palette = node("AXComboBox");
        palette.focusable_text = true;
        palette.options = vec!["vector.object.add-ellipse".into()];

        palette.expanded = Some(false);
        let closed = operations(&palette, false);
        assert!(closed.contains(&Operation::Click), "{closed:?}");
        assert!(closed.contains(&Operation::Select), "{closed:?}");

        palette.expanded = Some(true);
        let open = operations(&palette, false);
        assert!(!open.contains(&Operation::Click), "{open:?}");
        assert!(open.contains(&Operation::Select), "{open:?}");
        // Typing is how the list is filtered, so it survives either way.
        assert!(open.contains(&Operation::TypeText), "{open:?}");
    }

    #[test]
    fn a_menu_leaf_offers_only_menu() {
        let mut leaf = node("AXMenuItem");
        leaf.actions = vec!["AXPress".into()];
        assert_eq!(operations(&leaf, true), vec![Operation::Menu]);
    }

    #[test]
    fn a_disabled_control_offers_nothing() {
        let mut button = node("AXButton");
        button.enabled = false;
        assert!(operations(&button, false).is_empty());
        let mut leaf = node("AXMenuItem");
        leaf.enabled = false;
        assert!(operations(&leaf, true).is_empty());
    }

    #[test]
    fn display_role_deaxes_and_special_cases_secure_and_search_fields() {
        assert_eq!(display_role("AXButton", None), "button");
        assert_eq!(display_role("AXTextField", None), "textfield");
        assert_eq!(
            display_role("AXTextField", Some("AXSearchField")),
            "searchfield"
        );
        assert_eq!(
            display_role("AXTextField", Some("AXSecureTextField")),
            "securefield"
        );
        assert_eq!(display_role("AXSecureTextField", None), "securefield");
        assert_eq!(display_role("AXStaticText", None), "text");
        assert_eq!(display_role("AXRadioButton", Some("AXTabButton")), "tab");
        assert_eq!(display_role("AXWebArea", None), "webarea");
    }

    #[test]
    fn check_state_is_read_from_the_value_of_checkable_roles_only() {
        let mut check = node("AXCheckBox");
        check.value = Some("1".into());
        assert_eq!(state_of(&check).checked, Some(Checked::On));
        check.value = Some("2".into());
        assert_eq!(state_of(&check).checked, Some(Checked::Mixed));
        check.value = Some("0".into());
        assert_eq!(state_of(&check).checked, Some(Checked::Off));

        let mut slider = node("AXSlider");
        slider.value = Some("1".into());
        assert_eq!(state_of(&slider).checked, None);
    }

    #[test]
    fn tunnelling_skips_only_anonymous_actionless_wrappers() {
        let mut group = node("AXGroup");
        assert!(is_tunnel(&group));
        group.title = Some("Formatting".into());
        assert!(!is_tunnel(&group), "a labelled group is real structure");

        let mut pressable = node("AXGroup");
        pressable.actions = vec!["AXPress".into()];
        assert!(!is_tunnel(&pressable));
    }

    #[test]
    fn decorations_never_reach_the_table() {
        for role in ["AXScrollBar", "AXSplitter", "AXGrowArea"] {
            let mut n = node(role);
            n.actions = vec!["AXPress".into()];
            assert!(!is_table_worthy(&n), "{role} must be dropped");
        }
    }

    #[test]
    fn modal_detection_covers_sheets_and_dialog_subroles() {
        assert!(is_modal(&node("AXSheet")));
        let mut win = node("AXWindow");
        assert!(!is_modal(&win));
        win.subrole = Some("AXDialog".into());
        assert!(is_modal(&win));
    }
}
