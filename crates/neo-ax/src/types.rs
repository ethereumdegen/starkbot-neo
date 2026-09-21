//! The public data the AX actor hands out.
//!
//! Every type here is plain data. No `AXUIElement`, no `CFRetained`, nothing
//! `!Send`: elements stay on the actor thread and callers hold ids and
//! fingerprints only.

use serde::{Deserialize, Serialize};

/// A screen rectangle in global points, top-left origin of the primary display.
///
/// Never serialised out of the crate: Jev decides by index, never by geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Centre point of the rectangle.
    #[must_use]
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// True when the two rectangles share any area.
    #[must_use]
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// True when width and height are both positive.
    #[must_use]
    pub fn is_visible_size(&self) -> bool {
        self.w > 0.5 && self.h > 0.5
    }

    /// True when this rectangle fully encloses `inner` (a two-point slack
    /// absorbs the half-pixel rounding AX frames arrive with).
    #[must_use]
    pub fn contains_rect(&self, inner: &Rect) -> bool {
        const SLACK: f64 = 2.0;
        self.x - SLACK <= inner.x
            && self.y - SLACK <= inner.y
            && self.x + self.w + SLACK >= inner.x + inner.w
            && self.y + self.h + SLACK >= inner.y + inner.h
    }

    /// Distance the rectangle's origin moved, relative to its own size.
    ///
    /// Used by the guard: a move larger than the element itself is stale.
    #[must_use]
    pub fn moved_more_than_itself(&self, other: &Rect) -> bool {
        let dx = (self.x - other.x).abs();
        let dy = (self.y - other.y).abs();
        dx > self.w.max(1.0) || dy > self.h.max(1.0)
    }
}

/// A running application, as far as the actor is concerned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Localized name, e.g. `Mail`.
    pub name: String,
    /// Bundle identifier, when the app has one.
    pub bundle_id: Option<String>,
    /// Unix process id.
    pub pid: i32,
    /// Whether the app owns the menu bar right now.
    pub frontmost: bool,
}

/// How a caller names the app it wants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppSel {
    /// Whatever is frontmost at the moment the command runs.
    Frontmost,
    /// An exact process id.
    Pid(i32),
    /// An exact bundle identifier, compared case-insensitively.
    BundleId(String),
    /// A case-insensitive substring of the localized name.
    Name(String),
}

impl AppSel {
    /// Whether `app` satisfies this selector.
    #[must_use]
    pub fn matches(&self, app: &AppInfo) -> bool {
        match self {
            Self::Frontmost => app.frontmost,
            Self::Pid(pid) => app.pid == *pid,
            Self::BundleId(id) => app
                .bundle_id
                .as_deref()
                .is_some_and(|b| b.eq_ignore_ascii_case(id)),
            Self::Name(needle) => {
                let hay = app.name.to_lowercase();
                hay.contains(&needle.to_lowercase())
            }
        }
    }

    /// Whether `app` satisfies this selector once no app matched exactly.
    ///
    /// One case only: the caller's name is the app's own name plus a
    /// qualifier, like `"LibreOffice Calc"` for an app that calls itself
    /// *LibreOffice*. This is a real failure the eval suite caught — the
    /// model asked for `LibreOffice Calc`, [`Self::matches`] said no running
    /// application matched, and the agent concluded Calc was not installed
    /// while a spreadsheet sat open in front of it. Office suites, and
    /// anything whose modules share one process, hit this.
    ///
    /// It is deliberately a *second pass*: if any app matches exactly, that
    /// one wins. The remaining ambiguity is honest — asked for
    /// `"Safari Technology Preview"` while only `Safari` runs, this resolves
    /// to Safari. Falling back to the closest running app beats refusing, and
    /// the trace records which app was activated.
    #[must_use]
    pub fn matches_qualified(&self, app: &AppInfo) -> bool {
        let Self::Name(needle) = self else {
            return false;
        };
        let hay = app.name.to_lowercase();
        // A one- or two-character name would match almost anything.
        if hay.chars().count() < 3 {
            return false;
        }
        needle
            .to_lowercase()
            .strip_prefix(hay.as_str())
            .is_some_and(|rest| rest.starts_with(' '))
    }

    /// Pick the best match out of a list: an exact pid or bundle id wins, then
    /// the frontmost candidate, then the first in the list. A name that
    /// matched nothing exactly gets one more pass through
    /// [`Self::matches_qualified`].
    #[must_use]
    pub fn pick<'a>(&self, apps: &'a [AppInfo]) -> Option<&'a AppInfo> {
        self.pick_with(apps, |sel, app| sel.matches(app))
            .or_else(|| self.pick_with(apps, |sel, app| sel.matches_qualified(app)))
    }

    fn pick_with<'a>(
        &self,
        apps: &'a [AppInfo],
        predicate: impl Fn(&Self, &AppInfo) -> bool,
    ) -> Option<&'a AppInfo> {
        let mut matched = apps.iter().filter(|app| predicate(self, app));
        let first = matched.next()?;
        if first.frontmost {
            return Some(first);
        }
        match matched.find(|app| app.frontmost) {
            Some(front) => Some(front),
            None => Some(first),
        }
    }
}

impl std::fmt::Display for AppSel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Frontmost => f.write_str("the frontmost app"),
            Self::Pid(pid) => write!(f, "pid {pid}"),
            Self::BundleId(id) => write!(f, "bundle id {id}"),
            Self::Name(n) => write!(f, "name containing {n:?}"),
        }
    }
}

/// A handle onto one element of one observation.
///
/// Only ids: the `AXUIElement` it names never leaves the actor thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ref {
    /// The observation generation the index belongs to.
    pub generation: u32,
    /// Index into that observation's element table.
    pub index: u16,
}

/// Checkbox / radio / toggle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Checked {
    /// Checked.
    On,
    /// Unchecked.
    Off,
    /// Partially checked.
    Mixed,
}

/// Bare state words shown next to an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The control accepts input.
    pub enabled: bool,
    /// The control has keyboard focus.
    pub focused: bool,
    /// The control is selected (rows, tabs, cells).
    pub selected: bool,
    /// A disclosure/outline row is open.
    pub expanded: bool,
    /// Check state, for controls that have one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checked: Option<Checked>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            enabled: true,
            focused: false,
            selected: false,
            expanded: false,
            checked: None,
        }
    }
}

/// What Jev may ask for on one element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Operation {
    /// Press it.
    Click,
    /// Put text into it.
    TypeText,
    /// Choose one of `options`.
    Select,
    /// Invoke a menu-bar path.
    Menu,
}

/// Window-scoped controls offered alongside the element table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Control {
    /// Scroll the governing scroll area up.
    ScrollUp,
    /// Scroll the governing scroll area down.
    ScrollDown,
    /// Let the app settle and re-observe.
    Wait,
}

/// The window (or modal sheet) the observation describes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Window title, empty when the window has none.
    pub title: String,
    /// True when a sheet or modal dialog owns the observation.
    pub modal: bool,
    /// `AXURL` of a web area inside the window, when there is one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub url: Option<String>,
    /// Window frame in global points. Private: never serialised to Jev.
    #[serde(skip)]
    pub frame: Rect,
    /// Role-and-title fingerprint of the window. Private: never serialised.
    #[serde(skip)]
    pub(crate) fingerprint: String,
}

/// One row of the element table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Element {
    /// Stable index for this observation; the only thing the model may answer.
    pub index: u16,
    /// Lower-cased, de-`AX`-ed role: `button`, `textfield`, `securefield`.
    pub role: String,
    /// Best label for the element, possibly empty.
    pub label: String,
    /// Current value, at most 80 chars; `"empty"` for a blank text field.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value: Option<String>,
    /// Bare state words.
    pub state: State,
    /// Nearest labelled ancestor, e.g. `toolbar`, `sheet Discard draft?`, `row 3`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub container: Option<String>,
    /// What may be done with it. Empty means "visible but not actionable".
    pub operations: Vec<Operation>,
    /// Choices, for `SELECT` only.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub options: Vec<String>,

    /// Private handle onto the actor's element store. Never serialised.
    #[serde(skip)]
    pub(crate) r: Ref,
    /// Private frame in global points. Never serialised.
    #[serde(skip)]
    pub(crate) frame: Rect,
}

impl Element {
    /// The private ref for this row, for callers inside the crate boundary.
    #[must_use]
    pub fn reference(&self) -> Ref {
        self.r
    }
}

/// One atomic observation of an app: what Jev sees and may act on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ElementTable {
    /// The generation these indices belong to.
    pub generation: u32,
    /// The observed app.
    pub app: AppInfo,
    /// The observed window or modal.
    pub window: WindowInfo,
    /// Visible static text in reading order, at most 6,000 chars.
    pub text: String,
    /// At most 250 rows.
    pub elements: Vec<Element>,
    /// Window-scoped controls valid right now.
    pub controls: Vec<Control>,
    /// True when anything was cut to fit a budget.
    pub truncated: bool,
}

impl ElementTable {
    /// The row with that index, if it exists.
    #[must_use]
    pub fn element(&self, index: u16) -> Option<&Element> {
        self.elements.iter().find(|e| e.index == index)
    }

    /// The freshness contract for acting on one row of this observation.
    ///
    /// `jev-nav` builds this the moment it picks an index and hands it back
    /// with the action, so the actor can tell whether the surface still is
    /// the one the decision was made on.
    #[must_use]
    pub fn guard_for(&self, index: u16) -> Option<Guard> {
        let element = self.element(index)?;
        Some(Guard {
            generation: self.generation,
            pid: self.app.pid,
            window: self.window.fingerprint.clone(),
            modal: self.window.modal,
            target: Some(element.r),
            element: None,
            enabled: element.state.enabled,
            frame: element.frame,
        })
    }

    /// The freshness contract for an action with no element target, such as
    /// a menu path, a key or a scroll.
    #[must_use]
    pub fn guard_window(&self) -> Guard {
        Guard {
            generation: self.generation,
            pid: self.app.pid,
            window: self.window.fingerprint.clone(),
            modal: self.window.modal,
            target: None,
            element: None,
            enabled: true,
            frame: self.window.frame,
        }
    }
}

/// The identity of an element across re-walks, used for relocation and guards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint {
    /// `AXRole`.
    pub role: String,
    /// `AXSubrole`.
    pub subrole: Option<String>,
    /// Resolved label.
    pub label: String,
    /// `AXIdentifier`.
    pub identifier: Option<String>,
    /// Child-index path from the walk root.
    pub path: Vec<u16>,
    /// Label of the nearest labelled ancestor.
    pub container: Option<String>,
}

/// The freshness contract handed out with every executable action.
#[derive(Clone, Debug, PartialEq)]
pub struct Guard {
    /// Generation the observation belongs to.
    pub generation: u32,
    /// Pid of the observed app.
    pub pid: i32,
    /// Fingerprint of the observed window.
    pub window: String,
    /// Whether the observation was of a modal.
    pub modal: bool,
    /// The target element, when the action has one.
    pub target: Option<Ref>,
    /// Fingerprint of the target element.
    pub element: Option<Fingerprint>,
    /// Whether the target was enabled at observation time.
    pub enabled: bool,
    /// Frame of the target at observation time, in global points.
    pub frame: Rect,
}

/// A key from the fixed, chord-free key set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    /// Return / Enter.
    Return,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Space.
    Space,
    /// Delete / Backspace.
    Delete,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
}

/// A modifier that may be held while a key is pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    /// Shift.
    Shift,
    /// Control.
    Control,
    /// Option / Alt.
    Option,
    /// Command.
    Command,
}

/// Which way a scroll goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDir {
    /// Towards the start of the content.
    Up,
    /// Towards the end of the content.
    Down,
}

/// One thing the actor can do to an app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AxAction {
    /// Press an element (`AXPress` / `AXConfirm` / `AXPick` / `AXShowMenu`).
    Press {
        /// The target.
        target: Ref,
    },
    /// Put text into an element, verified by read-back.
    SetValue {
        /// The target.
        target: Ref,
        /// The value to write.
        text: String,
    },
    /// Choose one of a `SELECT` element's options.
    SelectOption {
        /// The target.
        target: Ref,
        /// The option label, as offered in `Element::options`.
        option: String,
    },
    /// Walk the menu bar by title path and press each level.
    SelectMenu {
        /// Full path, e.g. `["File", "Export", "PDF…"]`.
        path: Vec<String>,
    },
    /// Post one key, optionally with modifiers.
    Key {
        /// The key.
        key: Key,
        /// Modifiers held for the duration of the key.
        modifiers: Vec<Modifier>,
    },
    /// Type literal text into whatever has focus.
    TypeText {
        /// The text.
        text: String,
    },
    /// Scroll the governing scroll area by 80 % of its visible height.
    Scroll {
        /// Direction.
        direction: ScrollDir,
        /// Element whose scroll area governs; `None` uses the largest one.
        target: Option<Ref>,
    },
}

impl AxAction {
    /// The element this action targets, when it has one.
    #[must_use]
    pub fn target(&self) -> Option<Ref> {
        match self {
            Self::Press { target }
            | Self::SetValue { target, .. }
            | Self::SelectOption { target, .. } => Some(*target),
            Self::Scroll { target, .. } => *target,
            Self::SelectMenu { .. } | Self::Key { .. } | Self::TypeText { .. } => None,
        }
    }

    /// Short name used in error and trace lines.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Press { .. } => "press",
            Self::SetValue { .. } => "set_value",
            Self::SelectOption { .. } => "select_option",
            Self::SelectMenu { .. } => "select_menu",
            Self::Key { .. } => "key",
            Self::TypeText { .. } => "type_text",
            Self::Scroll { .. } => "scroll",
        }
    }
}

/// How an action reached the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Through the accessibility API.
    Ax,
    /// Through a synthetic `CGEvent`.
    CgEvent,
}

/// What an action did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActOutcome {
    /// Whether the action was dispatched at all.
    pub performed: bool,
    /// How it was dispatched.
    pub method: Method,
    /// Whether the target had to be relocated by fingerprint first.
    pub relocated: bool,
    /// One line for `recent_actions`.
    pub summary: String,
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests opt out of unwrap_used (05 §2)")]
mod tests {

    /// The eval failure that produced `matches_qualified`: the model names an
    /// app more specifically than the app names itself.
    #[test]
    fn a_qualified_name_resolves_to_the_suite_that_owns_it() {
        let libre = AppInfo {
            name: "LibreOffice".to_owned(),
            bundle_id: Some("org.libreoffice.script".to_owned()),
            pid: 1,
            frontmost: true,
        };
        let sel = AppSel::Name("LibreOffice Calc".to_owned());
        // Not an exact match — that is the bug this exists for.
        assert!(!sel.matches(&libre));
        assert!(sel.matches_qualified(&libre));
        assert_eq!(sel.pick(std::slice::from_ref(&libre)), Some(&libre));
    }

    /// An exact match always wins, so the fallback cannot steal a selection.
    #[test]
    fn an_exact_match_beats_a_qualified_one() {
        let libre = AppInfo {
            name: "LibreOffice".to_owned(),
            bundle_id: None,
            pid: 1,
            frontmost: false,
        };
        let calc = AppInfo {
            name: "LibreOffice Calc".to_owned(),
            bundle_id: None,
            pid: 2,
            frontmost: false,
        };
        let apps = vec![libre, calc.clone()];
        assert_eq!(
            AppSel::Name("LibreOffice Calc".to_owned()).pick(&apps),
            Some(&calc)
        );
    }

    /// A qualifier has to start at a word boundary, or every app whose name is
    /// a prefix of another would match.
    #[test]
    fn a_shared_prefix_without_a_boundary_does_not_match() {
        let pages = AppInfo {
            name: "Pag".to_owned(),
            bundle_id: None,
            pid: 3,
            frontmost: false,
        };
        assert!(!AppSel::Name("Pages".to_owned()).matches_qualified(&pages));
    }

    use super::*;

    fn app(name: &str, bundle: Option<&str>, pid: i32, frontmost: bool) -> AppInfo {
        AppInfo {
            name: name.to_owned(),
            bundle_id: bundle.map(ToOwned::to_owned),
            pid,
            frontmost,
        }
    }

    #[test]
    fn app_sel_matches_by_pid_bundle_and_name_substring() {
        let mail = app("Mail", Some("com.apple.mail"), 812, false);
        assert!(AppSel::Pid(812).matches(&mail));
        assert!(!AppSel::Pid(813).matches(&mail));
        assert!(AppSel::BundleId("COM.APPLE.MAIL".into()).matches(&mail));
        assert!(
            !AppSel::BundleId("com.apple.mai".into()).matches(&mail),
            "bundle id is exact"
        );
        assert!(
            AppSel::Name("ai".into()).matches(&mail),
            "name is a substring"
        );
        assert!(AppSel::Name("MAIL".into()).matches(&mail));
        assert!(!AppSel::Name("Notes".into()).matches(&mail));
        assert!(!AppSel::Frontmost.matches(&mail));
    }

    #[test]
    fn app_sel_without_bundle_id_never_matches_a_bundle_selector() {
        let anon = app("helper", None, 99, true);
        assert!(!AppSel::BundleId("com.example".into()).matches(&anon));
    }

    #[test]
    fn app_sel_pick_prefers_the_frontmost_match() {
        let apps = vec![
            app("Safari", Some("com.apple.Safari"), 1, false),
            app("Safari", Some("com.apple.Safari"), 2, true),
        ];
        assert_eq!(AppSel::Name("safari".into()).pick(&apps).unwrap().pid, 2);
        assert_eq!(AppSel::Pid(1).pick(&apps).unwrap().pid, 1);
        assert!(AppSel::Name("nope".into()).pick(&apps).is_none());
    }

    #[test]
    fn rect_geometry() {
        let a = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        let b = Rect {
            x: 99.0,
            y: 49.0,
            w: 10.0,
            h: 10.0,
        };
        let far = Rect {
            x: 200.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        assert!(a.intersects(&b));
        assert!(!a.intersects(&far));
        assert_eq!(a.center(), (50.0, 25.0));
        assert!(a.is_visible_size());
        assert!(!Rect::default().is_visible_size());
        // Moving less than its own size is not a stale move.
        assert!(!a.moved_more_than_itself(&Rect {
            x: 40.0,
            y: 10.0,
            w: 100.0,
            h: 50.0
        }));
        assert!(a.moved_more_than_itself(&Rect {
            x: 400.0,
            y: 0.0,
            w: 100.0,
            h: 50.0
        }));
    }

    #[test]
    fn action_target_and_name() {
        let r = Ref {
            generation: 3,
            index: 7,
        };
        assert_eq!(AxAction::Press { target: r }.target(), Some(r));
        assert_eq!(AxAction::TypeText { text: "hi".into() }.target(), None);
        assert_eq!(
            AxAction::Scroll {
                direction: ScrollDir::Up,
                target: None
            }
            .target(),
            None
        );
        assert_eq!(AxAction::SelectMenu { path: vec![] }.name(), "select_menu");
    }
}
