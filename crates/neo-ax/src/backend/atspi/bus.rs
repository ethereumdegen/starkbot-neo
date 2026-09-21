//! The AT-SPI2 transport: one D-Bus connection, one call at a time, each
//! with a deadline.
//!
//! An AT-SPI object is a `(bus name, object path)` pair and nothing else, so
//! unlike `AXUIElement` it is plain `Send + Sync` data — [`AxRef`] is two
//! reference-counted strings. What this module adds over `zbus` is the
//! per-call timeout, the small set of reads and writes 17 §3.1's mapping
//! table names, and three hard-won rules about how *not* to ask:
//!
//! * **Never `Properties.GetAll` on `org.a11y.atspi.Accessible`.** It folds
//!   `Name` and `Description` into one round trip and it is fatal:
//!   LibreOffice's ATK bridge lets the UNO
//!   `IllegalAccessibleComponentStateException` that `getLocale()` raises
//!   escape, `std::terminate` runs, and the whole application aborts.
//!   Measured on this machine: a walk that reads `Locale` kills Calc after
//!   241 nodes, every time. Named reads only, and never `Locale`.
//! * **Never `Action.GetActions`.** WebKitGTK does not answer it — the call
//!   simply never returns, so a Tauri window would hang the actor — and
//!   Chromium answers it with empty action names. `NActions` plus
//!   `GetName(i)` answers correctly everywhere and costs one extra call on
//!   the one node about to be pressed.
//! * **`Text.GetText(0, -1)`, not `GetText(0, n)`.** A range past the end
//!   silently returns the empty string, which reads as "this cell is blank"
//!   for a cell that is not.
//!
//! Every call is wrapped in a timeout because an unresponsive app must cost
//! one deadline, not the run: [`AxError::Unresponsive`] names it.

use std::sync::Arc;
use std::time::Duration;

use atspi::{InterfaceSet, Role, StateSet};
use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::error::AxError;
use crate::types::Rect;

/// `org.a11y.atspi.Accessible`.
pub(crate) const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const ACTION: &str = "org.a11y.atspi.Action";
const COMPONENT: &str = "org.a11y.atspi.Component";
const EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
const TEXT: &str = "org.a11y.atspi.Text";
const VALUE: &str = "org.a11y.atspi.Value";
const TABLE: &str = "org.a11y.atspi.Table";
const TABLE_CELL: &str = "org.a11y.atspi.TableCell";
const DOCUMENT: &str = "org.a11y.atspi.Document";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The registry's root object: its children are the running applications.
const REGISTRY: &str = "org.a11y.atspi.Registry";
const REGISTRY_ROOT: &str = "/org/a11y/atspi/accessible/root";

/// `ATSPI_COORD_TYPE_SCREEN`. Global points, as the macOS backend reports.
const COORD_SCREEN: u32 = 0;

/// How long one AT-SPI call may take before the app is called unresponsive.
///
/// The macOS backend sets a per-app messaging timeout for the same reason.
/// 250 ms is far above the measured cost of any single call on this machine
/// (29 µs) and far below the per-observation deadline.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_millis(250);

/// A handle onto one accessible object: a bus name and an object path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AxRef {
    /// Unique bus name of the application connection that owns the object.
    pub bus: Arc<str>,
    /// Object path within that connection.
    pub path: Arc<str>,
}

impl AxRef {
    /// A reference from the two strings AT-SPI hands out together.
    pub(crate) fn new(bus: &str, path: &str) -> Self {
        Self {
            bus: Arc::from(bus),
            path: Arc::from(path),
        }
    }

    /// The registry's root object, whose children are the applications.
    pub(crate) fn registry_root() -> Self {
        Self::new(REGISTRY, REGISTRY_ROOT)
    }

    /// Whether this is the null object AT-SPI returns for "no parent".
    pub(crate) fn is_null(&self) -> bool {
        self.path.as_ref() == "/org/a11y/atspi/null" || self.bus.is_empty()
    }
}

impl From<(String, OwnedObjectPath)> for AxRef {
    fn from((bus, path): (String, OwnedObjectPath)) -> Self {
        Self::new(&bus, path.as_str())
    }
}

/// The accessibility bus.
pub(crate) struct Bus {
    conn: Connection,
}

impl Bus {
    /// Connect to the accessibility bus named by `org.a11y.Bus.GetAddress`.
    ///
    /// # Errors
    ///
    /// [`AxError::NoBus`] when the session publishes no a11y bus, which is
    /// the one honest answer on a session with no `at-spi2-core`.
    pub(crate) async fn connect() -> Result<Self, AxError> {
        let conn = atspi::AccessibilityConnection::new()
            .await
            .map_err(|e| AxError::NoBus {
                detail: e.to_string(),
            })?;
        Ok(Self {
            conn: conn.connection().clone(),
        })
    }

    /// Tell the session an assistive client is listening.
    ///
    /// Chromium and Electron publish no tree at all while
    /// `org.a11y.Status.IsEnabled` is false, and on a bare Wayland session
    /// (no GNOME, no KDE) nothing sets it. This is the Linux counterpart of
    /// the macOS backend's `AXEnhancedUserInterface` write: best effort,
    /// repeated per observation, and discarded when it is refused.
    pub(crate) async fn announce(&self) {
        let _ = atspi::connection::set_session_accessibility(true).await;
    }

    async fn call<B, R>(
        &self,
        target: &AxRef,
        interface: &str,
        method: &str,
        body: &B,
    ) -> Result<R, AxError>
    where
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let path = ObjectPath::try_from(target.path.as_ref()).map_err(|_| AxError::Ax {
            call: "object path",
            code: 0,
        })?;
        let reply = tokio::time::timeout(
            CALL_TIMEOUT,
            self.conn.call_method(
                Some(target.bus.as_ref()),
                path,
                Some(interface),
                method,
                body,
            ),
        )
        .await
        .map_err(|_| AxError::Unresponsive {
            app: target.bus.to_string(),
        })?
        .map_err(|_| AxError::Ax {
            call: "call",
            code: 0,
        })?;
        reply.body().deserialize::<R>().map_err(|_| AxError::Ax {
            call: "decode",
            code: 0,
        })
    }

    /// One named property. Never `GetAll`, and never `Locale`.
    async fn property(&self, target: &AxRef, interface: &str, name: &str) -> Option<OwnedValue> {
        self.call(target, PROPERTIES, "Get", &(interface, name))
            .await
            .ok()
    }

    async fn string_property(&self, target: &AxRef, interface: &str, name: &str) -> Option<String> {
        let value = self.property(target, interface, name).await?;
        <&str>::try_from(&value).ok().map(ToOwned::to_owned)
    }

    async fn f64_property(&self, target: &AxRef, interface: &str, name: &str) -> Option<f64> {
        let value = self.property(target, interface, name).await?;
        f64::try_from(&value).ok()
    }

    async fn i32_property(&self, target: &AxRef, interface: &str, name: &str) -> Option<i32> {
        let value = self.property(target, interface, name).await?;
        i32::try_from(&value).ok()
    }

    // -- Accessible --------------------------------------------------------

    /// `Accessible.GetChildren`, in reading order.
    pub(crate) async fn children(&self, target: &AxRef) -> Vec<AxRef> {
        let kids: Vec<(String, OwnedObjectPath)> = self
            .call(target, ACCESSIBLE, "GetChildren", &())
            .await
            .unwrap_or_default();
        kids.into_iter().map(AxRef::from).collect()
    }

    /// `Accessible.GetRole`.
    pub(crate) async fn role(&self, target: &AxRef) -> Option<Role> {
        let raw: u32 = self.call(target, ACCESSIBLE, "GetRole", &()).await.ok()?;
        Role::try_from(raw).ok()
    }

    /// `Accessible.Name`.
    pub(crate) async fn name(&self, target: &AxRef) -> Option<String> {
        self.string_property(target, ACCESSIBLE, "Name").await
    }

    /// `Accessible.Description`.
    pub(crate) async fn description(&self, target: &AxRef) -> Option<String> {
        self.string_property(target, ACCESSIBLE, "Description")
            .await
    }

    /// `Accessible.AccessibleId`, the closest thing to `AXIdentifier`.
    pub(crate) async fn identifier(&self, target: &AxRef) -> Option<String> {
        self.string_property(target, ACCESSIBLE, "AccessibleId")
            .await
    }

    /// `Accessible.GetState`, decoded into the flag set.
    pub(crate) async fn states(&self, target: &AxRef) -> StateSet {
        let raw: Vec<u32> = self
            .call(target, ACCESSIBLE, "GetState", &())
            .await
            .unwrap_or_default();
        let bits = u64::from(raw.first().copied().unwrap_or(0))
            | (u64::from(raw.get(1).copied().unwrap_or(0)) << 32);
        StateSet::from_bits(bits).unwrap_or_else(|_| StateSet::empty())
    }

    /// `Accessible.GetInterfaces`.
    pub(crate) async fn interfaces(&self, target: &AxRef) -> InterfaceSet {
        self.call(target, ACCESSIBLE, "GetInterfaces", &())
            .await
            .unwrap_or_else(|_: AxError| InterfaceSet::empty())
    }

    /// `Accessible.Parent`.
    pub(crate) async fn parent(&self, target: &AxRef) -> Option<AxRef> {
        let value = self.property(target, ACCESSIBLE, "Parent").await?;
        let (bus, path) = <(String, OwnedObjectPath)>::try_from(value).ok()?;
        let parent = AxRef::new(&bus, path.as_str());
        (!parent.is_null()).then_some(parent)
    }

    // -- Component ---------------------------------------------------------

    /// `Component.GetExtents` in screen coordinates.
    pub(crate) async fn extents(&self, target: &AxRef) -> Option<Rect> {
        let (x, y, w, h): (i32, i32, i32, i32) = self
            .call(target, COMPONENT, "GetExtents", &(COORD_SCREEN))
            .await
            .ok()?;
        Some(Rect {
            x: f64::from(x),
            y: f64::from(y),
            w: f64::from(w),
            h: f64::from(h),
        })
    }

    /// `Component.GrabFocus`.
    pub(crate) async fn grab_focus(&self, target: &AxRef) -> bool {
        self.call(target, COMPONENT, "GrabFocus", &())
            .await
            .unwrap_or(false)
    }

    /// `Component.ScrollTo(ANYWHERE)` — bring the object into view without
    /// synthesising anything.
    pub(crate) async fn scroll_into_view(&self, target: &AxRef) -> bool {
        /// `ATSPI_SCROLL_ANYWHERE`.
        const ANYWHERE: u32 = 6;
        self.call(target, COMPONENT, "ScrollTo", &(ANYWHERE))
            .await
            .unwrap_or(false)
    }

    /// `Component.GetAccessibleAtPoint`, the hit test.
    pub(crate) async fn at_point(&self, target: &AxRef, x: i32, y: i32) -> Option<AxRef> {
        let hit: (String, OwnedObjectPath) = self
            .call(
                target,
                COMPONENT,
                "GetAccessibleAtPoint",
                &(x, y, COORD_SCREEN),
            )
            .await
            .ok()?;
        let found = AxRef::from(hit);
        (!found.is_null()).then_some(found)
    }

    // -- Action ------------------------------------------------------------

    /// The advertised action names, read one at a time.
    ///
    /// `Action.GetActions` would fetch them in one call and must not be used
    /// (see the module comment). Most objects offer exactly one action, so
    /// this is two round trips.
    pub(crate) async fn action_names(&self, target: &AxRef) -> Vec<String> {
        let Some(count) = self.i32_property(target, ACTION, "NActions").await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for index in 0..count.clamp(0, 8) {
            let name: String = self
                .call(target, ACTION, "GetName", &(index))
                .await
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            // Said in the canonical vocabulary here, at the edge, for the
            // same reason roles are: the policy sets downstream know only
            // `AX…` names, and a toolkit's own spelling reaching them is a
            // silently empty operation list.
            out.push(crate::mapping::canonical_action(&name).to_owned());
        }
        out
    }

    /// `Action.GetKeyBinding`, the accelerator of a menu item.
    pub(crate) async fn key_binding(&self, target: &AxRef) -> Option<String> {
        let binding: String = self
            .call(target, ACTION, "GetKeyBinding", &(0i32))
            .await
            .ok()?;
        let binding = binding.trim();
        // ATK packs three bindings into one string separated by `;`: the
        // mnemonic, the "full" path and the accelerator. Only the last is a
        // shortcut worth showing.
        let accelerator = binding.rsplit(';').find(|part| !part.trim().is_empty())?;
        (!accelerator.trim().is_empty()).then(|| accelerator.trim().to_owned())
    }

    /// `Action.DoAction`.
    pub(crate) async fn do_action(&self, target: &AxRef, index: i32) -> Result<bool, AxError> {
        self.call(target, ACTION, "DoAction", &(index)).await
    }

    // -- Text, Value, EditableText ----------------------------------------

    /// `Text.GetText(0, -1)` — the whole string. A bounded range past the
    /// end answers with the empty string instead of the content.
    pub(crate) async fn text(&self, target: &AxRef) -> Option<String> {
        self.call(target, TEXT, "GetText", &(0i32, -1i32))
            .await
            .ok()
    }
    /// `Document.GetAttributeValue("DocURL")`, the AT-SPI `AXURL`.
    pub(crate) async fn document_url(&self, target: &AxRef) -> Option<String> {
        let url: String = self
            .call(target, DOCUMENT, "GetAttributeValue", &("DocURL"))
            .await
            .ok()?;
        (!url.trim().is_empty()).then_some(url)
    }

    /// `TableCell.Position`, the `(row, column)` a cell sits at.
    pub(crate) async fn cell_position(&self, target: &AxRef) -> Option<(i32, i32)> {
        let value = self.property(target, TABLE_CELL, "Position").await?;
        <(i32, i32)>::try_from(value).ok()
    }

    /// `Value.CurrentValue`, normalised to 0..1 against its own bounds.
    ///
    /// The macOS `AXValue` of a scroll bar is already a fraction; AT-SPI
    /// reports the raw value with `MinimumValue`/`MaximumValue` beside it,
    /// and `table::scroll_affordance` expects the fraction.
    pub(crate) async fn value_fraction(&self, target: &AxRef) -> Option<f64> {
        let current = self.f64_property(target, VALUE, "CurrentValue").await?;
        let min = self
            .f64_property(target, VALUE, "MinimumValue")
            .await
            .unwrap_or(0.0);
        let max = self
            .f64_property(target, VALUE, "MaximumValue")
            .await
            .unwrap_or(0.0);
        let span = max - min;
        if span.abs() < f64::EPSILON {
            return Some(0.0);
        }
        Some(((current - min) / span).clamp(0.0, 1.0))
    }

    /// `Value.CurrentValue`, raw.
    pub(crate) async fn current_value(&self, target: &AxRef) -> Option<f64> {
        self.f64_property(target, VALUE, "CurrentValue").await
    }

    /// `Value.CurrentValue`, written.
    pub(crate) async fn set_current_value(&self, target: &AxRef, value: f64) -> bool {
        let variant = Value::from(value);
        self.call::<_, ()>(target, PROPERTIES, "Set", &(VALUE, "CurrentValue", variant))
            .await
            .is_ok()
    }

    /// `EditableText.SetTextContents`.
    pub(crate) async fn set_text(&self, target: &AxRef, text: &str) -> bool {
        self.call(target, EDITABLE_TEXT, "SetTextContents", &(text))
            .await
            .unwrap_or(false)
    }

    // -- Table -------------------------------------------------------------

    /// `Table.NRows` and `Table.NColumns`.
    pub(crate) async fn table_size(&self, target: &AxRef) -> Option<(i32, i32)> {
        let rows = self.i32_property(target, TABLE, "NRows").await?;
        let columns = self.i32_property(target, TABLE, "NColumns").await?;
        Some((rows, columns))
    }

    /// `Table.GetAccessibleAt`, the only way into a grid that manages its
    /// own descendants.
    pub(crate) async fn cell_at(&self, target: &AxRef, row: i32, column: i32) -> Option<AxRef> {
        let cell: (String, OwnedObjectPath) = self
            .call(target, TABLE, "GetAccessibleAt", &(row, column))
            .await
            .ok()?;
        let found = AxRef::from(cell);
        (!found.is_null()).then_some(found)
    }

    // -- Registry ----------------------------------------------------------

    /// The pid behind a bus name, asked of the accessibility bus daemon.
    ///
    /// AT-SPI's own `Application` interface has no process id; the owner of
    /// the connection does, and every application talks to the a11y bus
    /// directly, so this is the app's real pid.
    pub(crate) async fn pid_of(&self, bus: &str) -> Option<u32> {
        let reply: u32 = tokio::time::timeout(
            CALL_TIMEOUT,
            self.conn.call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetConnectionUnixProcessID",
                &(bus),
            ),
        )
        .await
        .ok()?
        .ok()?
        .body()
        .deserialize()
        .ok()?;
        Some(reply)
    }
}
