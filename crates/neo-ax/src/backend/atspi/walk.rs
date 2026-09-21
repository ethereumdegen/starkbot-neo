//! Turning an AT-SPI2 tree into the `RawNode`s the pure layers consume.
//!
//! The shape is the macOS walk's, and deliberately so: `table::build_table`
//! owns the 250-row budget, the diversity rule, the depth tunnelling and the
//! reading order, and it never learns which backend produced the tree.
//!
//! What differs is the transport, and 17 §3.1 asked for the difference to be
//! measured rather than guessed. It was (`plans/spikes.md`):
//!
//! * A **naive** walk — one property per D-Bus call, one node at a time —
//!   costs 2,029 nodes / 12,174 calls / 373 ms on a LibreOffice Calc frame,
//!   missing the 400 ms observation deadline on its own.
//! * Reading the five things every node needs (`GetRole`, `Name`,
//!   `GetState`, `GetInterfaces`, `GetChildren`) **together**, with siblings
//!   in flight at once, is 10,145 calls / 66 ms for the same frame. One
//!   round trip on the a11y bus costs ~29 µs.
//! * Everything else — extents, description, action names, text, value — is
//!   fetched **only for the nodes the role policy might keep**, which is the
//!   collection-time pruning §3.1 allows: ~250 survivors cost ~1,000 further
//!   calls and ~10 ms.
//!
//! The one thing genuinely cut at collection time is the **menu bar**, which
//! is walked separately into `MenuLeaf`s exactly as macOS walks it, because
//! on LibreOffice it is 1,447 of the frame's 2,029 nodes.

use std::collections::HashMap;
use std::time::Instant;

use atspi::{Interface, InterfaceSet, Role, State, StateSet};
use futures_util::future::join_all;

use super::bus::{AxRef, Bus};
use crate::mapping::{canonical_role, carries_text, is_table_worthy};
use crate::raw::{MenuLeaf, RawNode, collapse_whitespace};
use crate::types::Rect;

/// Node ceiling for one walk, as macOS (01 §Walk "Caps").
pub(crate) const NODE_BUDGET: usize = 1_500;
/// Depth ceiling before tunnelling. Generous because AT-SPI containers nest
/// hard: a GTK window is a dozen `Filler`s before the first real widget.
const DEPTH_LIMIT: usize = 60;
/// How many reads are in flight at once.
const FANOUT: usize = 32;
/// Menu leaves read per observation. LibreOffice publishes 1,286.
const MENU_BUDGET: usize = 1_200;
/// How deep a menu path may go.
const MENU_DEPTH: usize = 8;
/// Cells expanded out of a grid that manages its own descendants.
const CELL_BUDGET: usize = 120;
/// The grid window used when the corner hit test cannot size one.
const FALLBACK_ROWS: i32 = 12;
/// Columns to match [`FALLBACK_ROWS`].
const FALLBACK_COLUMNS: i32 = 8;

/// One walked window.
pub(crate) struct Walk {
    /// The window subtree, ready for `table::build_table`.
    pub root: RawNode,
    /// URL of a web area inside the window, when there is one.
    pub url: Option<String>,
    /// Whether a budget or the deadline cut the walk short.
    pub truncated: bool,
}

/// What one structural read yields.
#[derive(Clone)]
struct Shallow {
    role: Role,
    states: StateSet,
    interfaces: InterfaceSet,
    name: String,
    children: Vec<AxRef>,
}

/// What the second pass adds to a node the role policy might keep.
#[derive(Default)]
struct Extras {
    frame: Rect,
    description: Option<String>,
    identifier: Option<String>,
    value: Option<String>,
    actions: Vec<String>,
}

/// The walker: owns the element store for one observation.
pub(crate) struct Walker<'a> {
    bus: &'a Bus,
    /// `RawNode::id` -> the object it came from.
    store: Vec<AxRef>,
    /// `RawNode::id` -> the interfaces that object advertises.
    interfaces: Vec<InterfaceSet>,
    deadline: Instant,
    truncated: bool,
    url: Option<String>,
}

impl<'a> Walker<'a> {
    /// A walker with a wall deadline.
    pub(crate) fn new(bus: &'a Bus, deadline: Instant) -> Self {
        Self {
            bus,
            store: Vec::new(),
            interfaces: Vec::new(),
            deadline,
            truncated: false,
            url: None,
        }
    }

    /// The element store this walk built: `RawNode::id` indexes it.
    pub(crate) fn into_store(self) -> Vec<AxRef> {
        self.store
    }

    fn out_of_time(&self) -> bool {
        Instant::now() >= self.deadline
    }

    async fn shallow(&self, target: &AxRef) -> Shallow {
        let (role, name, states, interfaces, children) = futures_util::join!(
            self.bus.role(target),
            self.bus.name(target),
            self.bus.states(target),
            self.bus.interfaces(target),
            self.bus.children(target),
        );
        Shallow {
            role: role.unwrap_or(Role::Invalid),
            states,
            interfaces,
            name: name.unwrap_or_default(),
            children,
        }
    }

    fn node_of(&mut self, target: &AxRef, shallow: &Shallow) -> RawNode {
        let id = u32::try_from(self.store.len()).unwrap_or(u32::MAX);
        self.store.push(target.clone());
        self.interfaces.push(shallow.interfaces);
        let (role, subrole) = canonical_role(shallow.role, shallow.states);
        let title = collapse_whitespace(shallow.name.trim());
        RawNode {
            id,
            role: role.to_owned(),
            subrole: subrole.map(ToOwned::to_owned),
            title: (!title.is_empty()).then_some(title),
            description: None,
            value: checked_value(role, shallow.states),
            placeholder: None,
            help: None,
            identifier: None,
            url: None,
            enabled: shallow.states.contains(State::Enabled),
            focused: shallow.states.contains(State::Focused),
            selected: shallow.states.contains(State::Selected),
            expanded: shallow
                .states
                .contains(State::Expandable)
                .then(|| shallow.states.contains(State::Expanded)),
            frame: Rect::default(),
            // Refined by the second pass for the nodes it visits. An object
            // that implements `Action` advertises at least one action in
            // practice, and `Action.GetActions` cannot be used to check
            // (WebKitGTK never answers it), so the structural pass assumes a
            // press and the second pass replaces it with the real names.
            actions: if shallow.interfaces.contains(Interface::Action) {
                vec!["AXPress".to_owned()]
            } else {
                Vec::new()
            },
            settable_value: shallow.interfaces.contains(Interface::EditableText)
                || (shallow.interfaces.contains(Interface::Value)
                    && shallow.states.contains(State::Editable)),
            options: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Walk one window subtree and fill in everything the table needs.
    pub(crate) async fn walk_window(&mut self, window: &AxRef) -> Walk {
        let mut budget = NODE_BUDGET;
        let shallow = self.shallow(window).await;
        let mut root = self.node_of(window, &shallow);
        root.children = Box::pin(self.children_of(window, &shallow, 0, &mut budget)).await;
        self.second_pass(&mut root).await;
        fill_options(&mut root);
        Walk {
            root,
            url: self.url.take(),
            truncated: self.truncated,
        }
    }

    /// Walk a dialog, hung under the window it covers.
    ///
    /// AT-SPI publishes a dialog as another *top-level* of the application,
    /// not as a child of the window it covers — the opposite of an
    /// `AXSheet`. `table::build_table` looks for the modal among the
    /// window's children and gives it the whole budget, so the two are
    /// handed over in that shape: the window read shallowly for its title
    /// and frame, the dialog walked in full beneath it.
    pub(crate) async fn walk_with_modal(&mut self, window: &AxRef, dialog: &AxRef) -> Walk {
        let mut root = self.read_shallow(window).await;
        let inner = Box::pin(self.walk_window(dialog)).await;
        root.children = vec![inner.root];
        Walk {
            root,
            url: inner.url,
            truncated: inner.truncated,
        }
    }

    /// Read one object without its subtree, complete enough for a guard.
    pub(crate) async fn read_shallow(&mut self, target: &AxRef) -> RawNode {
        let shallow = self.shallow(target).await;
        let mut node = self.node_of(target, &shallow);
        let extras = self
            .one_extras(target, shallow.interfaces, &node.role)
            .await;
        apply(&mut node, extras);
        node
    }

    /// Depth-first, siblings concurrently.
    async fn children_of(
        &mut self,
        parent: &AxRef,
        shallow: &Shallow,
        depth: usize,
        budget: &mut usize,
    ) -> Vec<RawNode> {
        if self.url.is_none() {
            let (role, _) = canonical_role(shallow.role, shallow.states);
            if role == "AXWebArea" {
                self.url = self.bus.document_url(parent).await;
            }
        }

        // A grid that manages its own descendants publishes no children at
        // all: LibreOffice Calc answers `GetChildren` with nothing and
        // `NRows`/`NColumns` with the whole address space. The cells are
        // real and reachable one at a time, so the visible window of them is
        // expanded here, bounded.
        if shallow.states.contains(State::ManagesDescendants)
            && shallow.children.is_empty()
            && shallow.interfaces.contains(Interface::Table)
        {
            return Box::pin(self.expand_grid(parent, budget)).await;
        }

        // The menu bar is walked separately, into `MenuLeaf`s with their
        // full paths, exactly as macOS does. Left in place it is 1,447 of
        // LibreOffice's 2,029 nodes and would eat the node budget before the
        // toolbar was reached.
        let (parent_role, _) = canonical_role(shallow.role, shallow.states);
        if parent_role == "AXMenuBar" || depth >= DEPTH_LIMIT {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(shallow.children.len());
        for chunk in shallow.children.chunks(FANOUT) {
            if *budget == 0 || self.out_of_time() {
                self.truncated = true;
                break;
            }
            let reads = join_all(chunk.iter().map(|child| self.shallow(child))).await;
            for (child, read) in chunk.iter().zip(reads) {
                if *budget == 0 {
                    self.truncated = true;
                    break;
                }
                *budget -= 1;
                let mut node = self.node_of(child, &read);
                node.children = Box::pin(self.children_of(child, &read, depth + 1, budget)).await;
                out.push(node);
            }
        }
        out
    }

    /// The visible window of a grid that manages its own descendants.
    ///
    /// The grid's own extents plus two hit tests give the corner cells;
    /// everything between them is what the user is looking at. A spreadsheet
    /// reports 1,048,576 rows, so asking for "the table" is not an option
    /// and neither is guessing a fixed range.
    async fn expand_grid(&mut self, table: &AxRef, budget: &mut usize) -> Vec<RawNode> {
        let Some(frame) = self.bus.extents(table).await else {
            return Vec::new();
        };
        if !frame.is_visible_size() {
            return Vec::new();
        }
        let Some((rows, columns)) = self.bus.table_size(table).await else {
            return Vec::new();
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen coordinates are far inside i32"
        )]
        let point = |x: f64, y: f64| (x as i32, y as i32);
        let (x0, y0) = point(frame.x + 2.0, frame.y + 2.0);
        let (x1, y1) = point(frame.x + frame.w - 3.0, frame.y + frame.h - 3.0);
        let (first, last) = futures_util::join!(
            self.bus.at_point(table, x0, y0),
            self.bus.at_point(table, x1, y1)
        );
        let origin = match first {
            Some(cell) => self.bus.cell_position(&cell).await.unwrap_or((0, 0)),
            None => (0, 0),
        };
        let corner = match last {
            Some(cell) => self.bus.cell_position(&cell).await,
            None => None,
        };
        let (r0, c0) = (origin.0.max(0), origin.1.max(0));
        // The hit test is the accurate answer and the fallback is not: on
        // LibreOffice the bottom-right corner of the grid's own extents
        // lands outside any addressed cell and answers with the cell under
        // the origin, which would leave a spreadsheet showing exactly one
        // cell. A fixed window then stands in, sized to what macOS gets out
        // of the same document (01 §Field notes: 92 addressable cells).
        let (r1, c1) = match corner {
            Some((r, c)) if r > r0 || c > c0 => (r, c),
            _ => (r0 + FALLBACK_ROWS - 1, c0 + FALLBACK_COLUMNS - 1),
        };
        let r1 = r1.clamp(r0, rows.saturating_sub(1));
        let c1 = c1.clamp(c0, columns.saturating_sub(1));

        let mut out = Vec::new();
        let mut addresses = Vec::new();
        'rows: for row in r0..=r1 {
            for column in c0..=c1 {
                if addresses.len() >= CELL_BUDGET || addresses.len() >= *budget {
                    self.truncated = true;
                    break 'rows;
                }
                addresses.push((row, column));
            }
        }
        for chunk in addresses.chunks(FANOUT) {
            if self.out_of_time() {
                self.truncated = true;
                break;
            }
            let cells = join_all(
                chunk
                    .iter()
                    .map(|(row, column)| self.bus.cell_at(table, *row, *column)),
            )
            .await;
            let found: Vec<AxRef> = cells.into_iter().flatten().collect();
            let reads = join_all(found.iter().map(|cell| self.shallow(cell))).await;
            for (cell, read) in found.iter().zip(reads) {
                let mut node = self.node_of(cell, &read);
                // A spreadsheet cell is a text field, the way macOS reports
                // it (01 §Field notes): the grid publishes no `EditableText`
                // and its value is written by focusing and typing.
                if node.role == "AXCell" && read.interfaces.contains(Interface::Text) {
                    node.role = "AXTextField".to_owned();
                }
                *budget = budget.saturating_sub(1);
                out.push(node);
            }
        }
        out
    }

    /// Fetch the rest of every node the role policy might keep.
    async fn second_pass(&mut self, root: &mut RawNode) {
        let mut wanted: Vec<(u32, AxRef, InterfaceSet, String)> = Vec::new();
        collect_extras(root, &self.store, &self.interfaces, &mut wanted);
        let mut found: HashMap<u32, Extras> = HashMap::with_capacity(wanted.len());
        let mut cut = false;
        {
            // A shared borrow so the per-node futures can be built inside a
            // closure; nothing in this pass mutates the walker.
            let me: &Self = self;
            for chunk in wanted.chunks(FANOUT) {
                if me.out_of_time() {
                    cut = true;
                    break;
                }
                let read = join_all(chunk.iter().map(
                    |(id, target, interfaces, role)| async move {
                        (*id, me.one_extras(target, *interfaces, role).await)
                    },
                ))
                .await;
                found.extend(read);
            }
        }
        self.truncated |= cut;
        apply_tree(root, &mut found);
    }

    async fn one_extras(&self, target: &AxRef, interfaces: InterfaceSet, role: &str) -> Extras {
        let frame = async {
            if interfaces.contains(Interface::Component) {
                self.bus.extents(target).await
            } else {
                None
            }
        };
        let actions = async {
            if interfaces.contains(Interface::Action) {
                self.bus.action_names(target).await
            } else {
                Vec::new()
            }
        };
        let value = async {
            match role {
                // A check state is carried by `State::checked`, which the
                // structural pass already wrote into `value`.
                "AXCheckBox" | "AXRadioButton" | "AXMenuItem" | "AXDisclosureTriangle" => None,
                "AXScrollBar" => self
                    .bus
                    .value_fraction(target)
                    .await
                    .map(|fraction| fraction.to_string()),
                "AXSlider" | "AXProgressIndicator" | "AXIncrementor" => {
                    self.bus.current_value(target).await.map(|v| v.to_string())
                }
                _ if interfaces.contains(Interface::Text) => self.bus.text(target).await,
                _ if interfaces.contains(Interface::Value) => {
                    self.bus.current_value(target).await.map(|v| v.to_string())
                }
                _ => None,
            }
        };
        let (frame, actions, value, description, identifier) = futures_util::join!(
            frame,
            actions,
            value,
            self.bus.description(target),
            self.bus.identifier(target),
        );
        Extras {
            frame: frame.unwrap_or_default(),
            description: description.filter(|d| !d.trim().is_empty()),
            identifier: identifier.filter(|i| !i.trim().is_empty()),
            value: value.filter(|v| !v.is_empty()),
            actions,
        }
    }

    /// The menu bar, flattened to leaves with their full paths.
    ///
    /// Read without opening anything, as on macOS: AT-SPI publishes the
    /// whole menu tree whether or not a menu is on screen.
    pub(crate) async fn walk_menu_bar(&mut self, window: &AxRef) -> Vec<MenuLeaf> {
        let Some(bar) = Box::pin(self.find_menu_bar(window, 0)).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut path = Vec::new();
        Box::pin(self.menu_level(&bar, &mut path, &mut out)).await;
        out
    }

    async fn find_menu_bar(&self, from: &AxRef, depth: usize) -> Option<AxRef> {
        if depth > DEPTH_LIMIT || self.out_of_time() {
            return None;
        }
        let children = self.bus.children(from).await;
        let roles = join_all(children.iter().map(|child| self.bus.role(child))).await;
        for (child, role) in children.iter().zip(&roles) {
            if *role == Some(Role::MenuBar) {
                return Some(child.clone());
            }
        }
        for child in &children {
            if let Some(found) = Box::pin(self.find_menu_bar(child, depth + 1)).await {
                return Some(found);
            }
        }
        None
    }

    async fn menu_level(
        &mut self,
        parent: &AxRef,
        path: &mut Vec<String>,
        out: &mut Vec<MenuLeaf>,
    ) {
        if path.len() > MENU_DEPTH || self.out_of_time() {
            self.truncated = true;
            return;
        }
        let children = self.bus.children(parent).await;
        for chunk in children.chunks(FANOUT) {
            let reads = join_all(chunk.iter().map(|child| self.shallow(child))).await;
            for (child, read) in chunk.iter().zip(reads) {
                if out.len() >= MENU_BUDGET {
                    self.truncated = true;
                    return;
                }
                let (role, _) = canonical_role(read.role, read.states);
                if role == "AXSplitter" {
                    continue;
                }
                let label = collapse_whitespace(read.name.trim());
                // GTK and VCL both wrap a submenu in one more `Menu` object
                // carrying the same name as the item that opens it, so the
                // label is pushed only when it says something new.
                let repeated = path.last().is_some_and(|last| *last == label);
                let pushed = !label.is_empty() && !repeated;
                if pushed {
                    path.push(label.clone());
                }
                if read.children.is_empty() {
                    if !path.is_empty() {
                        let id = u32::try_from(self.store.len()).unwrap_or(u32::MAX);
                        self.store.push(child.clone());
                        self.interfaces.push(read.interfaces);
                        let shortcut = if read.interfaces.contains(Interface::Action) {
                            self.bus.key_binding(child).await
                        } else {
                            None
                        };
                        out.push(MenuLeaf {
                            id,
                            path: path.clone(),
                            shortcut,
                            enabled: read.states.contains(State::Enabled),
                        });
                    }
                } else {
                    Box::pin(self.menu_level(child, path, out)).await;
                }
                if pushed {
                    path.pop();
                }
            }
        }
    }
}

/// `State::Checked` in the form `mapping::state_of` parses.
fn checked_value(role: &str, states: StateSet) -> Option<String> {
    if !matches!(
        role,
        "AXCheckBox" | "AXRadioButton" | "AXMenuItem" | "AXDisclosureTriangle"
    ) {
        return None;
    }
    if states.contains(State::Indeterminate) {
        return Some("2".to_owned());
    }
    Some(
        if states.contains(State::Checked) || states.contains(State::Pressed) {
            "1"
        } else {
            "0"
        }
        .to_owned(),
    )
}

/// Which nodes earn the second pass.
///
/// Everything the role policy might keep, everything that contributes text,
/// the scroll bars `table::scroll_affordance` reads and the window itself —
/// no more, because each one costs three to five D-Bus round trips.
fn collect_extras(
    node: &RawNode,
    store: &[AxRef],
    interfaces: &[InterfaceSet],
    out: &mut Vec<(u32, AxRef, InterfaceSet, String)>,
) {
    let slot = node.id as usize;
    let wanted = is_table_worthy(node)
        || carries_text(node)
        || node.role == "AXScrollBar"
        || node.role == "AXWindow";
    if wanted && let (Some(target), Some(set)) = (store.get(slot), interfaces.get(slot)) {
        out.push((node.id, target.clone(), *set, node.role.clone()));
    }
    for child in &node.children {
        collect_extras(child, store, interfaces, out);
    }
}

fn apply(node: &mut RawNode, extras: Extras) {
    node.frame = extras.frame;
    node.description = extras.description;
    node.identifier = extras.identifier;
    if node.value.is_none() {
        node.value = extras.value;
    }
    if extras.actions.is_empty() {
        // The interface was advertised and answered with nothing, so the row
        // must not claim CLICK on an action that does not exist.
        node.actions.clear();
    } else {
        node.actions = extras.actions;
    }
}

fn apply_tree(node: &mut RawNode, extras: &mut HashMap<u32, Extras>) {
    if let Some(found) = extras.remove(&node.id) {
        apply(node, found);
    }
    for child in &mut node.children {
        apply_tree(child, extras);
    }
}

/// Fill `options` for the controls whose choices are already on the tree.
///
/// `mapping::operations` offers SELECT only when the choices are known, and
/// AT-SPI publishes a tab strip's tabs and an open combo box's items as
/// ordinary children, so they cost nothing extra.
fn fill_options(node: &mut RawNode) {
    if matches!(node.role.as_str(), "AXTabGroup" | "AXComboBox" | "AXList") {
        node.options = node
            .children
            .iter()
            .filter(|child| {
                matches!(
                    child.role.as_str(),
                    "AXRadioButton" | "AXRow" | "AXMenuItem" | "AXCell"
                )
            })
            .map(RawNode::label)
            .filter(|label| !label.is_empty())
            .collect();
    }
    for child in &mut node.children {
        fill_options(child);
    }
}
