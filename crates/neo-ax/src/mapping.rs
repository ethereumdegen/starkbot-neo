//! The `AX role → operation` mapping of `plans/01-accessibility.md`.
//!
//! Pure: it reads a `RawNode` and answers what Jev may do with it. No AX call
//! happens here, so every row of the table in the plan is unit-testable.

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
    if (TEXT_ROLES.contains(&role) || editable_web) && (node.settable_value || node.focused) {
        ops.push(Operation::TypeText);
    }

    // SELECT only when the choices are already known: a pop-up button that
    // hides its menu until it is opened stays a CLICK target.
    if SELECT_ROLES.contains(&role) && !node.options.is_empty() {
        ops.push(Operation::Select);
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
