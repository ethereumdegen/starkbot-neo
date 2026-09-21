//! The thin safe layer over the raw AX bindings.
//!
//! Everything in this module runs on the actor thread and nothing in it is
//! `Send`: [`AxElem`] wraps an `AXUIElement`, which must never leave that
//! thread. The module turns free `unsafe` C functions into `Result`s, and it
//! is one of the two places (with the `catch_unwind` in `actor`) where that
//! conversion happens.

#![allow(unsafe_code)]

use std::ptr::NonNull;
use std::time::Instant;

use objc2::exception::catch;
use objc2_application_services::{
    AXCopyMultipleAttributeOptions, AXError, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFNumber, CFRetained, CFString, CFType, CFURL, CGPoint, CGSize, Type,
};

use crate::error::AxError as Error;
use crate::raw::{MenuLeaf, RawNode, collapse_whitespace};
use crate::types::Rect;

/// How long a single app is given to answer one message.
pub(crate) const APP_MESSAGING_TIMEOUT: f32 = 1.0;
/// Process-wide default, so one hung app cannot freeze the actor.
pub(crate) const GLOBAL_MESSAGING_TIMEOUT: f32 = 2.0;
/// Hard ceiling on nodes kept by one walk.
pub(crate) const WALK_NODE_CAP: usize = 1_500;
/// Hard ceiling on menu leaves read per walk.
pub(crate) const MENU_LEAF_CAP: usize = 400;
/// How deep submenus are followed.
pub(crate) const MENU_DEPTH: usize = 3;

/// An `AXUIElement`. Not `Send`, never public, never cloned out of the actor.
pub(crate) struct AxElem(CFRetained<AXUIElement>);

impl AxElem {
    /// The application element for a pid.
    pub(crate) fn app(pid: i32) -> Self {
        // SAFETY: `AXUIElementCreateApplication` accepts any pid and returns a
        // +1 element (it does not validate that the process exists; every
        // later call on a dead pid simply returns an AXError).
        Self(unsafe { AXUIElement::new_application(pid) })
    }

    /// The system-wide element, used for hit tests and global settings.
    pub(crate) fn system_wide() -> Self {
        // SAFETY: nullary constructor, always returns a valid +1 element.
        Self(unsafe { AXUIElement::new_system_wide() })
    }

    fn from_retained(inner: CFRetained<AXUIElement>) -> Self {
        Self(inner)
    }

    /// Cap how long this element's app may take to answer one message.
    pub(crate) fn set_messaging_timeout(&self, seconds: f32) {
        // SAFETY: the element is valid and the timeout is a plain float; the
        // call only stores a per-element (or, for the system-wide element,
        // per-process) timeout.
        let _ = unsafe { self.0.set_messaging_timeout(seconds) };
    }

    /// The pid that owns this element.
    pub(crate) fn pid(&self) -> Option<i32> {
        let mut pid: i32 = 0;
        // SAFETY: `pid` points at a live local `i32` for the whole call, which
        // is exactly the binding's documented requirement.
        let err = unsafe { self.0.pid(NonNull::from(&mut pid)) };
        (err == AXError::Success).then_some(pid)
    }

    /// One attribute, or `None` when the app does not offer it.
    pub(crate) fn attr(&self, name: &CFString) -> Option<CFRetained<CFType>> {
        let mut out: *const CFType = std::ptr::null();
        // SAFETY: `out` is a live local pointer slot for the whole call. On
        // success the callee writes a +1 CFTypeRef into it, which we adopt
        // below; on failure it leaves it untouched and we never read it.
        let err = unsafe { self.0.copy_attribute_value(name, NonNull::from(&mut out)) };
        if err != AXError::Success {
            return None;
        }
        let ptr = NonNull::new(out.cast_mut())?;
        // SAFETY: `AXUIElementCopyAttributeValue` follows the Copy rule, so
        // the returned reference is +1 and ours to own.
        Some(unsafe { CFRetained::from_raw(ptr) })
    }

    /// Whether the attribute can be written.
    pub(crate) fn settable(&self, name: &CFString) -> bool {
        let mut settable: u8 = 0;
        // SAFETY: `settable` is a live local `Boolean` slot for the call.
        let err = unsafe { self.0.is_attribute_settable(name, NonNull::from(&mut settable)) };
        err == AXError::Success && settable != 0
    }

    /// Write an attribute.
    pub(crate) fn set_attr(&self, name: &CFString, value: &CFType) -> Result<(), Error> {
        // SAFETY: both arguments are live CF objects of the type the attribute
        // expects (the caller picks the pair); the callee only reads them.
        let err = unsafe { self.0.set_attribute_value(name, value) };
        ax_result("AXUIElementSetAttributeValue", err)
    }

    /// Many attributes in one IPC round trip.
    ///
    /// Slots the app rejected come back as `None` rather than failing the
    /// whole fetch, which is what makes the batched walk usable on apps with
    /// patchy trees.
    pub(crate) fn multi(&self, names: &CFArray<CFString>) -> Option<Vec<Option<CFRetained<CFType>>>> {
        let mut out: *const CFArray = std::ptr::null();
        // SAFETY: `names` is a CFArray of CFStrings, the generic type the
        // binding's contract requires, and `out` is a live local slot. The
        // zero option means "do not stop on the first error", so the returned
        // array always has one entry per requested attribute.
        let err = unsafe {
            self.0.copy_multiple_attribute_values(
                names.as_opaque(),
                AXCopyMultipleAttributeOptions::empty(),
                NonNull::from(&mut out),
            )
        };
        if err != AXError::Success {
            return None;
        }
        let ptr = NonNull::new(out.cast_mut())?;
        // SAFETY: the Copy rule again: the array is +1. Re-typing the generic
        // parameter is sound because `CFArray<T>` is a zero-sized-marker
        // wrapper around the same CFArrayRef, and the array does hold
        // CFTypeRefs.
        let array: CFRetained<CFArray<CFType>> = unsafe { CFRetained::from_raw(ptr.cast()) };

        let mut values = Vec::with_capacity(array.len());
        for i in 0..array.len() {
            let Some(item) = array.get(i) else {
                values.push(None);
                continue;
            };
            // An attribute the app could not answer comes back as an AXValue
            // wrapping the AXError. Those are holes, not values.
            if item.downcast_ref::<AXValue>().is_some_and(|v| {
                // SAFETY: `v` is a live AXValue; reading its type tag has no
                // preconditions.
                (unsafe { v.r#type() }) == AXValueType::AXError
            }) {
                values.push(None);
            } else {
                values.push(Some(item));
            }
        }
        Some(values)
    }

    /// The action names the element advertises.
    pub(crate) fn actions(&self) -> Vec<String> {
        let mut out: *const CFArray = std::ptr::null();
        // SAFETY: `out` is a live local slot; on success it receives a +1
        // CFArrayRef of CFStrings.
        let err = unsafe { self.0.copy_action_names(NonNull::from(&mut out)) };
        if err != AXError::Success {
            return Vec::new();
        }
        let Some(ptr) = NonNull::new(out.cast_mut()) else {
            return Vec::new();
        };
        // SAFETY: Copy rule; the array holds CFStrings.
        let array: CFRetained<CFArray<CFString>> = unsafe { CFRetained::from_raw(ptr.cast()) };
        (0..array.len()).filter_map(|i| array.get(i).map(|s| s.to_string())).collect()
    }

    /// Perform one action by name.
    pub(crate) fn perform(&self, action: &CFString) -> Result<(), Error> {
        // SAFETY: both the element and the action name are live; the call has
        // no other precondition.
        let err = unsafe { self.0.perform_action(action) };
        ax_result("AXUIElementPerformAction", err)
    }

    /// The deepest element at a global point. Only valid on the system-wide
    /// element or on an application element.
    pub(crate) fn element_at(&self, x: f64, y: f64) -> Option<AxElem> {
        let mut out: *const AXUIElement = std::ptr::null();
        // SAFETY: `out` is a live local slot, as the binding requires; the
        // coordinates are plain floats.
        let err = unsafe {
            self.0.copy_element_at_position(x as f32, y as f32, NonNull::from(&mut out))
        };
        if err != AXError::Success {
            return None;
        }
        let ptr = NonNull::new(out.cast_mut())?;
        // SAFETY: Copy rule: the element is +1.
        Some(Self(unsafe { CFRetained::from_raw(ptr) }))
    }

    /// Whether two handles name the same UI element.
    pub(crate) fn same_as(&self, other: &AxElem) -> bool {
        std::ptr::eq(std::ptr::from_ref(&*self.0), std::ptr::from_ref(&*other.0))
    }

    /// Another handle onto the same element, for the actor's own stores.
    pub(crate) fn duplicate(&self) -> AxElem {
        Self(self.0.clone())
    }
}

/// Turn a raw `AXError` into a `Result` with a named call site.
fn ax_result(call: &'static str, err: AXError) -> Result<(), Error> {
    match err {
        AXError::Success => Ok(()),
        AXError::CannotComplete => Err(Error::Unresponsive { app: call.to_owned() }),
        AXError::InvalidUIElement => Err(Error::StaleRef),
        other => Err(Error::Ax { call, code: other.0 }),
    }
}

/// Run a cluster of AX calls with Objective-C exceptions turned into errors.
///
/// Misbehaving apps and the CF↔ObjC bridge can raise, and an unwind across
/// FFI would abort the process.
pub(crate) fn guarded<R>(app: &str, call: &'static str, f: impl FnOnce() -> R) -> Result<R, Error> {
    catch(std::panic::AssertUnwindSafe(f))
        .map_err(|_| Error::Exception { app: app.to_owned(), call })
}

// ---------------------------------------------------------------------------
// CFType decoding
// ---------------------------------------------------------------------------

/// Render a CF value as a string, for the attributes Jev reads.
pub(crate) fn as_string(value: &CFType) -> Option<String> {
    if let Some(s) = value.downcast_ref::<CFString>() {
        return Some(s.to_string());
    }
    if let Some(b) = value.downcast_ref::<CFBoolean>() {
        return Some(if b.as_bool() { "1" } else { "0" }.to_owned());
    }
    if let Some(n) = value.downcast_ref::<CFNumber>() {
        return n.as_i64().map(|v| v.to_string()).or_else(|| n.as_f64().map(|v| v.to_string()));
    }
    if let Some(u) = value.downcast_ref::<CFURL>() {
        return Some(u.string().to_string());
    }
    None
}

/// Read a CF value as a boolean.
pub(crate) fn as_bool(value: &CFType) -> Option<bool> {
    if let Some(b) = value.downcast_ref::<CFBoolean>() {
        return Some(b.as_bool());
    }
    value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64).map(|v| v != 0)
}

/// Read a CF value as a floating-point number.
pub(crate) fn as_f64(value: &CFType) -> Option<f64> {
    value.downcast_ref::<CFNumber>().and_then(CFNumber::as_f64)
}

/// Read an `AXValue` holding a `CGPoint`.
fn as_point(value: &CFType) -> Option<(f64, f64)> {
    let ax = value.downcast_ref::<AXValue>()?;
    let mut point = CGPoint { x: 0.0, y: 0.0 };
    // SAFETY: the out pointer points at a live `CGPoint` and the requested
    // type tag matches it, which is what `AXValueGetValue` needs. It returns
    // false and writes nothing when the tag does not match the stored value.
    let ok = unsafe {
        ax.value(AXValueType::CGPoint, NonNull::from(&mut point).cast())
    };
    ok.then_some((point.x, point.y))
}

/// Read an `AXValue` holding a `CGSize`.
fn as_size(value: &CFType) -> Option<(f64, f64)> {
    let ax = value.downcast_ref::<AXValue>()?;
    let mut size = CGSize { width: 0.0, height: 0.0 };
    // SAFETY: as above, with a live `CGSize` and the matching type tag.
    let ok = unsafe { ax.value(AXValueType::CGSize, NonNull::from(&mut size).cast()) };
    ok.then_some((size.width, size.height))
}

/// Read a CF value as a list of elements.
fn as_elements(value: &CFType) -> Vec<AxElem> {
    let Some(array) = value.downcast_ref::<CFArray>() else {
        return Vec::new();
    };
    // SAFETY: re-typing the generic marker of a CFArray is sound (see `multi`)
    // and AX child arrays hold AXUIElementRefs; `downcast` below still checks
    // each element's CFTypeID, so a lying app yields an empty list, not UB.
    let typed: &CFArray<CFType> = unsafe { &*std::ptr::from_ref(array).cast() };
    (0..typed.len())
        .filter_map(|i| typed.get(i))
        .filter_map(|item| item.downcast::<AXUIElement>().ok())
        .map(AxElem::from_retained)
        .collect()
}

// ---------------------------------------------------------------------------
// Attribute name cache
// ---------------------------------------------------------------------------

/// The `CFString`s the walk uses, created once per actor thread so no node
/// fetch allocates an attribute name.
pub(crate) struct Attrs {
    pub(crate) role: CFRetained<CFString>,
    pub(crate) subrole: CFRetained<CFString>,
    pub(crate) title: CFRetained<CFString>,
    pub(crate) value: CFRetained<CFString>,
    pub(crate) enabled: CFRetained<CFString>,
    pub(crate) children: CFRetained<CFString>,
    pub(crate) visible_children: CFRetained<CFString>,
    pub(crate) focused_window: CFRetained<CFString>,
    /// `AXMainWindow` and `AXWindows`: what an app still answers when nothing
    /// is key. A development build launched from a terminal is on screen and
    /// never focused, and reading only `AXFocusedWindow` made it unobservable.
    pub(crate) main_window: CFRetained<CFString>,
    pub(crate) windows: CFRetained<CFString>,
    pub(crate) menu_bar: CFRetained<CFString>,
    pub(crate) parent: CFRetained<CFString>,
    /// `AXFocused`, for the focus-then-type fallback of `set_value`.
    pub(crate) focused: CFRetained<CFString>,
    /// `AXEnhancedUserInterface` and `AXManualAccessibility`: the two switches
    /// an app may require before it publishes its real tree.
    pub(crate) enhanced_ui: CFRetained<CFString>,
    pub(crate) manual_accessibility: CFRetained<CFString>,
    /// The bulk-fetch list, in [`slot`] order.
    pub(crate) node_list: CFRetained<CFArray<CFString>>,
    /// The bulk-fetch list for menu items.
    pub(crate) menu_list: CFRetained<CFArray<CFString>>,
}

impl Attrs {
    pub(crate) fn new() -> Self {
        let s = CFString::from_static_str;
        let role = s("AXRole");
        let subrole = s("AXSubrole");
        let title = s("AXTitle");
        let description = s("AXDescription");
        let value = s("AXValue");
        let placeholder = s("AXPlaceholderValue");
        let help = s("AXHelp");
        let identifier = s("AXIdentifier");
        let enabled = s("AXEnabled");
        let focused = s("AXFocused");
        let selected = s("AXSelected");
        let expanded = s("AXExpanded");
        let position = s("AXPosition");
        let size = s("AXSize");
        let url = s("AXURL");
        let children = s("AXChildren");
        let cmd_char = s("AXMenuItemCmdChar");
        let cmd_modifiers = s("AXMenuItemCmdModifiers");

        // Order must match `slot`: one IPC round trip answers all of them.
        let node_list = CFArray::from_objects(&[
            &*role,
            &*subrole,
            &*title,
            &*description,
            &*value,
            &*placeholder,
            &*help,
            &*identifier,
            &*enabled,
            &*focused,
            &*selected,
            &*expanded,
            &*position,
            &*size,
            &*url,
        ]);
        let menu_list = CFArray::from_objects(&[
            &*title,
            &*enabled,
            &*children,
            &*cmd_char,
            &*cmd_modifiers,
        ]);

        Self {
            role,
            subrole,
            title,
            value,
            enabled,
            children,
            visible_children: s("AXVisibleChildren"),
            focused_window: s("AXFocusedWindow"),
            main_window: s("AXMainWindow"),
            windows: s("AXWindows"),
            menu_bar: s("AXMenuBar"),
            parent: s("AXParent"),
            focused: s("AXFocused"),
            enhanced_ui: s("AXEnhancedUserInterface"),
            manual_accessibility: s("AXManualAccessibility"),
            node_list,
            menu_list,
        }
    }
}

/// Field order of [`Attrs::node_list`].
mod slot {
    pub(super) const ROLE: usize = 0;
    pub(super) const SUBROLE: usize = 1;
    pub(super) const TITLE: usize = 2;
    pub(super) const DESCRIPTION: usize = 3;
    pub(super) const VALUE: usize = 4;
    pub(super) const PLACEHOLDER: usize = 5;
    pub(super) const HELP: usize = 6;
    pub(super) const IDENTIFIER: usize = 7;
    pub(super) const ENABLED: usize = 8;
    pub(super) const FOCUSED: usize = 9;
    pub(super) const SELECTED: usize = 10;
    pub(super) const EXPANDED: usize = 11;
    pub(super) const POSITION: usize = 12;
    pub(super) const SIZE: usize = 13;
    pub(super) const URL: usize = 14;
}

// ---------------------------------------------------------------------------
// Walking
// ---------------------------------------------------------------------------

/// The result of one window walk: a pure tree plus the element store the ids
/// index into.
pub(crate) struct Walk {
    /// The pruned-but-not-yet-budgeted tree.
    pub(crate) root: RawNode,
    /// `RawNode::id` -> element. Stays on the actor thread.
    pub(crate) store: Vec<AxElem>,
    /// The first web-area URL seen, if any.
    pub(crate) url: Option<String>,
}

/// Read one node's attributes in a single IPC round trip.
fn read_node(elem: &AxElem, attrs: &Attrs, id: u32, app: &str) -> RawNode {
    let mut node = RawNode { id, enabled: true, ..RawNode::default() };
    let Ok(Some(values)) = guarded(app, "copy_multiple_attribute_values", || {
        elem.multi(&attrs.node_list)
    }) else {
        return node;
    };
    let get = |i: usize| values.get(i).and_then(Option::as_ref);

    node.role = get(slot::ROLE).and_then(|v| as_string(v)).unwrap_or_default();
    node.subrole = get(slot::SUBROLE).and_then(|v| as_string(v));
    node.title = get(slot::TITLE).and_then(|v| as_string(v));
    node.description = get(slot::DESCRIPTION).and_then(|v| as_string(v));
    node.placeholder = get(slot::PLACEHOLDER).and_then(|v| as_string(v));
    node.help = get(slot::HELP).and_then(|v| as_string(v));
    node.identifier = get(slot::IDENTIFIER).and_then(|v| as_string(v));
    node.url = get(slot::URL).and_then(|v| as_string(v));
    node.enabled = get(slot::ENABLED).and_then(|v| as_bool(v)).unwrap_or(true);
    node.focused = get(slot::FOCUSED).and_then(|v| as_bool(v)).unwrap_or(false);
    node.selected = get(slot::SELECTED).and_then(|v| as_bool(v)).unwrap_or(false);
    node.expanded = get(slot::EXPANDED).and_then(|v| as_bool(v));

    // A secure field's value is dropped here, before it is stored anywhere.
    if !node.is_secure() {
        node.value = get(slot::VALUE).and_then(|v| as_string(v)).map(|v| collapse_whitespace(&v));
    }

    let position = get(slot::POSITION).and_then(|v| as_point(v)).unwrap_or((0.0, 0.0));
    let size = get(slot::SIZE).and_then(|v| as_size(v)).unwrap_or((0.0, 0.0));
    node.frame = Rect { x: position.0, y: position.1, w: size.0, h: size.1 };

    node.actions = guarded(app, "copy_action_names", || elem.actions()).unwrap_or_default();
    node.settable_value = guarded(app, "is_attribute_settable", || elem.settable(&attrs.value))
        .unwrap_or(false);
    node.options = enumerable_options(elem, attrs, &node, app);
    node
}

/// Options a control exposes *without opening anything*.
fn enumerable_options(elem: &AxElem, attrs: &Attrs, node: &RawNode, app: &str) -> Vec<String> {
    const OPTION_PARENTS: [&str; 4] =
        ["AXRadioGroup", "AXTabGroup", "AXSegmentedControl", "AXComboBox"];
    if !OPTION_PARENTS.contains(&node.role.as_str()) {
        return Vec::new();
    }
    let Ok(children) = guarded(app, "options", || {
        elem.attr(&attrs.children).map(|v| as_elements(&v)).unwrap_or_default()
    }) else {
        return Vec::new();
    };
    children
        .iter()
        .take(64)
        .filter_map(|child| {
            guarded(app, "option_title", || child.attr(&attrs.title))
                .ok()
                .flatten()
                .and_then(|v| as_string(&v))
        })
        .map(|s| collapse_whitespace(&s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Walk a window subtree into a `RawNode` tree plus its element store.
pub(crate) fn walk(root: &AxElem, attrs: &Attrs, app: &str, deadline: Instant) -> Walk {
    let mut store: Vec<AxElem> = Vec::new();
    let mut url = None;
    let root_node = walk_into(root, attrs, app, deadline, 0, &mut store, &mut url);
    Walk { root: root_node, store, url }
}

fn walk_into(
    elem: &AxElem,
    attrs: &Attrs,
    app: &str,
    deadline: Instant,
    depth: usize,
    store: &mut Vec<AxElem>,
    url: &mut Option<String>,
) -> RawNode {
    let id = u32::try_from(store.len()).unwrap_or(u32::MAX);
    let mut node = read_node(elem, attrs, id, app);
    // The element store is indexed by `RawNode::id`; push after reading so the
    // id is the slot we just reserved.
    store.push(AxElem(elem.0.clone()));

    if url.is_none() && node.role == "AXWebArea" {
        url.clone_from(&node.url);
    }

    if depth >= 40 || store.len() >= WALK_NODE_CAP || Instant::now() >= deadline {
        return node;
    }

    // Prefer visible children where the role has them (tables, lists,
    // outlines): a 5,000-row table must not be walked in full.
    let visible = matches!(node.role.as_str(), "AXTable" | "AXOutline" | "AXList");
    let children = guarded(app, "children", || {
        let attr = if visible { &attrs.visible_children } else { &attrs.children };
        elem.attr(attr)
            .or_else(|| elem.attr(&attrs.children))
            .map(|v| as_elements(&v))
            .unwrap_or_default()
    })
    .unwrap_or_default();

    for child in &children {
        if store.len() >= WALK_NODE_CAP || Instant::now() >= deadline {
            break;
        }
        node.children.push(walk_into(child, attrs, app, deadline, depth + 1, store, url));
    }
    node
}

/// Walk the menu bar into leaves, opening nothing.
pub(crate) fn walk_menu_bar(
    app_elem: &AxElem,
    attrs: &Attrs,
    app: &str,
    store: &mut Vec<AxElem>,
    deadline: Instant,
) -> Vec<MenuLeaf> {
    // `AXMenuBar` answers with the bar element itself; its children are the
    // `AXMenuBarItem`s.
    let Ok(Some(bar)) = guarded(app, "menu_bar", || {
        app_elem.attr(&attrs.menu_bar).and_then(|v| first_element(&v))
    }) else {
        return Vec::new();
    };
    let Ok(items) = guarded(app, "menu_bar_items", || {
        bar.attr(&attrs.children).map(|v| as_elements(&v)).unwrap_or_default()
    }) else {
        return Vec::new();
    };
    let mut leaves = Vec::new();
    for item in items {
        if leaves.len() >= MENU_LEAF_CAP || Instant::now() >= deadline {
            break;
        }
        menu_into(&item, attrs, app, &mut Vec::new(), &mut leaves, store, 0, deadline);
    }
    leaves
}

#[expect(
    clippy::too_many_arguments,
    reason = "a recursive walker threading its accumulators; splitting it would hide the recursion"
)]
fn menu_into(
    item: &AxElem,
    attrs: &Attrs,
    app: &str,
    path: &mut Vec<String>,
    leaves: &mut Vec<MenuLeaf>,
    store: &mut Vec<AxElem>,
    depth: usize,
    deadline: Instant,
) {
    if depth > MENU_DEPTH || leaves.len() >= MENU_LEAF_CAP || Instant::now() >= deadline {
        return;
    }
    let Ok(Some(values)) = guarded(app, "menu_item", || item.multi(&attrs.menu_list)) else {
        return;
    };
    let title = values
        .first()
        .and_then(Option::as_ref)
        .and_then(|v| as_string(v))
        .map(|t| collapse_whitespace(&t))
        .unwrap_or_default();
    if title.is_empty() {
        return; // separator
    }
    let enabled = values.get(1).and_then(Option::as_ref).and_then(|v| as_bool(v)).unwrap_or(true);
    let children = values.get(2).and_then(Option::as_ref).map(|v| as_elements(v)).unwrap_or_default();

    path.push(title);

    // A menu item's single `AXMenu` child holds the submenu.
    let submenu: Vec<AxElem> = children
        .into_iter()
        .flat_map(|menu| {
            guarded(app, "submenu", || {
                menu.attr(&attrs.children).map(|v| as_elements(&v)).unwrap_or_default()
            })
            .unwrap_or_default()
        })
        .collect();

    if submenu.is_empty() {
        let cmd_char = values.get(3).and_then(Option::as_ref).and_then(|v| as_string(v));
        let modifiers = values
            .get(4)
            .and_then(Option::as_ref)
            .and_then(|v| as_f64(v))
            .map_or(0, |v| v as u32);
        let id = u32::try_from(store.len()).unwrap_or(u32::MAX);
        store.push(AxElem(item.0.clone()));
        leaves.push(MenuLeaf {
            id,
            path: path.clone(),
            shortcut: crate::raw::render_shortcut(cmd_char.as_deref(), modifiers),
            enabled,
        });
    } else {
        for child in &submenu {
            menu_into(child, attrs, app, path, leaves, store, depth + 1, deadline);
        }
    }

    path.pop();
}

/// Build a `CFString` from a Rust string, for one-off attribute writes.
/// A `CFBoolean` true, for setting a boolean attribute.
pub(crate) fn cf_true() -> &'static CFBoolean {
    CFBoolean::new(true)
}

pub(crate) fn cf(s: &str) -> CFRetained<CFString> {
    CFString::from_str(s)
}

/// The first element in an attribute value, whether it is one element or a
/// one-element array. `AXFocusedWindow` answers with a bare element; some
/// apps answer with an array.
pub(crate) fn first_element(value: &CFType) -> Option<AxElem> {
    if let Some(elem) = value.downcast_ref::<AXUIElement>() {
        return Some(AxElem(elem.retain()));
    }
    as_elements(value).into_iter().next()
}

/// Read only the attributes a guard compares: role, label, enabled, frame.
pub(crate) fn read_shallow(elem: &AxElem, attrs: &Attrs, app: &str) -> RawNode {
    let mut node = RawNode { enabled: true, ..RawNode::default() };
    let Ok(Some(values)) = guarded(app, "guard_refetch", || elem.multi(&attrs.node_list)) else {
        return node;
    };
    let get = |i: usize| values.get(i).and_then(Option::as_ref);
    node.role = get(slot::ROLE).and_then(|v| as_string(v)).unwrap_or_default();
    node.subrole = get(slot::SUBROLE).and_then(|v| as_string(v));
    node.title = get(slot::TITLE).and_then(|v| as_string(v));
    node.description = get(slot::DESCRIPTION).and_then(|v| as_string(v));
    node.placeholder = get(slot::PLACEHOLDER).and_then(|v| as_string(v));
    node.help = get(slot::HELP).and_then(|v| as_string(v));
    node.identifier = get(slot::IDENTIFIER).and_then(|v| as_string(v));
    node.enabled = get(slot::ENABLED).and_then(|v| as_bool(v)).unwrap_or(true);
    if !node.is_secure() {
        node.value = get(slot::VALUE).and_then(|v| as_string(v)).map(|v| collapse_whitespace(&v));
    }
    let position = get(slot::POSITION).and_then(|v| as_point(v)).unwrap_or((0.0, 0.0));
    let size = get(slot::SIZE).and_then(|v| as_size(v)).unwrap_or((0.0, 0.0));
    node.frame = Rect { x: position.0, y: position.1, w: size.0, h: size.1 };
    node
}

/// Whether a sheet or modal dialog is attached to this window right now.
///
/// Direct children only, matching `table::find_modal` exactly so the guard
/// and the observation cannot disagree about what "modal" means.
pub(crate) fn has_modal_child(window: &AxElem, attrs: &Attrs, app: &str) -> bool {
    let Ok(children) = guarded(app, "modal_children", || {
        window.attr(&attrs.children).map(|v| as_elements(&v)).unwrap_or_default()
    }) else {
        return false;
    };
    children.iter().take(64).any(|child| {
        let node = guarded(app, "modal_role", || RawNode {
            role: child.attr(&attrs.role).and_then(|v| as_string(&v)).unwrap_or_default(),
            subrole: child.attr(&attrs.subrole).and_then(|v| as_string(&v)),
            ..RawNode::default()
        });
        node.is_ok_and(|n| crate::mapping::is_modal(&n))
    })
}

/// Whether the hit-test answer is the target, a descendant of it, or an
/// ancestor within `hops` `AXParent` steps.
pub(crate) fn is_same_or_related(
    hit: &AxElem,
    target: &AxElem,
    attrs: &Attrs,
    hops: usize,
) -> bool {
    if hit.same_as(target) {
        return true;
    }
    // Walk up from the hit: a click on a button's label answers with the
    // label, whose ancestor is the button we meant.
    let mut current = hit.duplicate();
    for _ in 0..hops {
        let Some(parent) = current.attr(&attrs.parent).and_then(|v| first_element(&v)) else {
            break;
        };
        if parent.same_as(target) {
            return true;
        }
        current = parent;
    }
    // Walk up from the target: a hit on a container that owns the target.
    let mut current = target.duplicate();
    for _ in 0..hops {
        let Some(parent) = current.attr(&attrs.parent).and_then(|v| first_element(&v)) else {
            break;
        };
        if parent.same_as(hit) {
            return true;
        }
        current = parent;
    }
    false
}

/// Press the child whose `AXTitle` matches, for `SELECT`.
pub(crate) fn press_child_titled(parent: &AxElem, attrs: &Attrs, title: &str) -> bool {
    let Some(children) = parent.attr(&attrs.children).map(|v| as_elements(&v)) else {
        return false;
    };
    let press = CFString::from_static_str("AXPress");
    for child in children.iter().take(128) {
        let child_title =
            child.attr(&attrs.title).and_then(|v| as_string(&v)).map(|t| collapse_whitespace(&t));
        if child_title.as_deref() == Some(title) {
            return child.perform(&press).is_ok();
        }
    }
    false
}

/// Walk `AXMenuBar` by title path and press the leaf.
///
/// Every level is verified to exist and to be enabled before the walk goes
/// on, so a path that cannot complete fails before anything is pressed.
///
/// Only the **leaf** is pressed. `AXPress` on an intermediate level opens
/// that menu on screen, and measured on TextEdit the subsequent leaf press
/// then did not take effect and left the menu open; submenu children are
/// readable while closed, so descending without opening is both correct and
/// tidier.
pub(crate) fn press_menu_path(
    app_elem: &AxElem,
    attrs: &Attrs,
    path: &[String],
) -> Result<(), Error> {
    let bar = app_elem
        .attr(&attrs.menu_bar)
        .and_then(|v| first_element(&v))
        .ok_or(Error::Unsupported("this app exposes no menu bar"))?;
    let press = CFString::from_static_str("AXPress");
    let mut level: Vec<AxElem> =
        bar.attr(&attrs.children).map(|v| as_elements(&v)).unwrap_or_default();

    for (depth, wanted) in path.iter().enumerate() {
        let found = level.iter().find(|item| {
            item.attr(&attrs.title)
                .and_then(|v| as_string(&v))
                .map(|t| collapse_whitespace(&t))
                .as_deref()
                == Some(wanted.as_str())
        });
        let Some(item) = found else {
            return Err(Error::Unsupported("that menu path does not exist"));
        };
        let enabled = item.attr(&attrs.enabled).and_then(|v| as_bool(&v)).unwrap_or(true);
        if !enabled {
            return Err(Error::Unsupported("that menu item is disabled"));
        }
        if depth + 1 == path.len() {
            item.perform(&press)?;
            return Ok(());
        }
        // Descend into the submenu the press just opened.
        let submenu: Vec<AxElem> = item
            .attr(&attrs.children)
            .map(|v| as_elements(&v))
            .unwrap_or_default()
            .into_iter()
            .flat_map(|menu| {
                menu.attr(&attrs.children).map(|v| as_elements(&v)).unwrap_or_default()
            })
            .collect();
        if submenu.is_empty() {
            return Err(Error::Unsupported("that menu has no submenu to descend into"));
        }
        level = submenu;
    }
    Ok(())
}
