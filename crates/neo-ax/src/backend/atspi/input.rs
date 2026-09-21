//! The input fallback: `zwp_virtual_keyboard_v1` and
//! `zwp_virtual_keyboard_v1`, spoken in-process (17 §3.3).
//!
//! **There is no virtual pointer here, on purpose.** A run shares the machine
//! with the person at it, and a synthetic click cannot be shared: it warps the
//! cursor out from under their hand, and under `follow_mouse` it drags window
//! focus along with it. The keyboard takes only the focused window, which is a
//! thing a person can work around. So the pointer protocol is not bound, not
//! held, and not reachable — the guarantee is the absence of the code, not a
//! rule someone has to remember.
//!
//! AT-SPI actions come first, exactly as on macOS. This is what happens when
//! an element advertises no usable action, or when a field takes typed text
//! but refuses a written value — which on Linux is the common case, not the
//! exotic one: **WebKitGTK implements no `EditableText` interface at all**,
//! so every `<input>` in a Tauri window is typed into rather than written
//! to, and a LibreOffice Calc cell has always been.
//!
//! `xdotool` is X11-only. `ydotool` needs a root daemon on `/dev/uinput`.
//! Both would also mean spawning a program to press a key, which is a shell
//! in everything but name (P3). Refused; this talks the protocols.
//!
//! # The keymap
//!
//! A virtual keyboard has no layout until it is given one, and the layout is
//! what turns a keycode into a character. So each burst of typing uploads a
//! keymap holding exactly the symbols it is about to send, one per keycode,
//! all at level 1 — no shift games, no dependence on the user's own layout,
//! and nothing typed that was not asked for. Modifiers for a chord are set
//! through `modifiers()` rather than by pressing a modifier key, which is
//! what the protocol is for.

use std::io::Write;
use std::os::fd::AsFd;
use std::time::{SystemTime, UNIX_EPOCH};

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

use crate::error::AxError;
use crate::types::{Key, Modifier};

/// `BTN_LEFT` from `linux/input-event-codes.h`.
/// `XKB_KEYMAP_FORMAT_TEXT_V1`.
const KEYMAP_TEXT_V1: u32 = 1;
/// evdev keycode = xkb keycode − 8.
const EVDEV_OFFSET: u32 = 8;
/// Symbols per uploaded keymap. Wayland key codes are evdev codes and the
/// compositor adds 8, so staying well inside 255 keeps every keycode legal.
const KEYMAP_PAGE: usize = 96;
/// Most characters [`VirtualInput::clear_field`] will delete. A form field
/// holding more than this is not a field the navigator should be rewriting
/// keystroke by keystroke.
const MAX_CLEAR: usize = 512;
/// How long a freshly uploaded keymap is given to reach the focused client
/// before keycodes are posted against it. Paid only when the layout actually
/// changes, which is once per write, not once per keystroke.
const KEYMAP_SETTLE: std::time::Duration = std::time::Duration::from_millis(60);

/// Modifier bits, in the order an X11 keymap assigns them.
const MOD_SHIFT: u32 = 1;
const MOD_CONTROL: u32 = 1 << 2;
const MOD_ALT: u32 = 1 << 3;
const MOD_SUPER: u32 = 1 << 6;

/// The Wayland connection and the two managers, bound once.
pub(crate) struct VirtualInput {
    /// Which compositor this is, so a missing protocol names its host.
    compositor: &'static str,
    queue: EventQueue<Sink>,
    handle: QueueHandle<Sink>,
    seat: WlSeat,
    keyboard: Option<ZwpVirtualKeyboardV1>,
    keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    /// The layout currently uploaded, so a repeat burst skips the upload.
    uploaded: Vec<String>,
}

/// The event sink. Nothing here needs an event: the virtual devices are
/// write-only and the seat's capabilities do not matter.
struct Sink;

delegate_noop!(Sink: ignore WlSeat);
delegate_noop!(Sink: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(Sink: ignore ZwpVirtualKeyboardV1);

impl Dispatch<WlRegistry, GlobalListContents> for Sink {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _handle: &QueueHandle<Self>,
    ) {
    }
}

fn now_ms() -> u32 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    u32::try_from(millis & u128::from(u32::MAX)).unwrap_or(0)
}

impl VirtualInput {
    /// Connect to the compositor and bind what it offers.
    ///
    /// # Errors
    ///
    /// [`AxError::NoVirtualInput`] when there is no Wayland display. A
    /// compositor that offers neither protocol still connects: the refusal
    /// then names the protocol the action needed, which is more useful than
    /// "input unavailable".
    pub(crate) fn connect(compositor: &'static str) -> Result<Self, AxError> {
        let conn = Connection::connect_to_env().map_err(|e| AxError::NoVirtualInput {
            detail: format!("no Wayland display: {e}"),
        })?;
        let (globals, queue): (GlobalList, EventQueue<Sink>) =
            registry_queue_init(&conn).map_err(|e| AxError::NoVirtualInput {
                detail: format!("the Wayland registry did not answer: {e}"),
            })?;
        let handle = queue.handle();
        let seat: WlSeat =
            globals
                .bind(&handle, 1..=9, ())
                .map_err(|e| AxError::NoVirtualInput {
                    detail: format!("no seat: {e}"),
                })?;
        let keyboard_manager = globals.bind(&handle, 1..=1, ()).ok();
        Ok(Self {
            compositor,
            queue,
            handle,
            seat,
            keyboard: None,
            keyboard_manager,
            uploaded: Vec::new(),
        })
    }

    fn flush(&mut self) {
        let _ = self.queue.roundtrip(&mut Sink);
    }

    fn keyboard(&mut self) -> Result<ZwpVirtualKeyboardV1, AxError> {
        if let Some(keyboard) = &self.keyboard {
            return Ok(keyboard.clone());
        }
        let manager = self
            .keyboard_manager
            .as_ref()
            .ok_or_else(|| AxError::NoVirtualInput {
                detail: format!(
                    "{} does not implement zwp_virtual_keyboard_v1",
                    self.compositor
                ),
            })?;
        let keyboard = manager.create_virtual_keyboard(&self.seat, &self.handle, ());
        self.keyboard = Some(keyboard.clone());
        Ok(keyboard)
    }

    /// Upload a layout holding exactly `symbols`, unless it is already up.
    fn upload(&mut self, symbols: &[String]) -> Result<(), AxError> {
        if self.uploaded == symbols {
            return Ok(());
        }
        // A *new* keyboard for every layout, because a focused client is
        // only told a keymap when one is first delivered to it. Replacing
        // the layout on a keyboard the client has already seen leaves that
        // client decoding the new keycodes with the old table: measured on
        // degen-paint Studio, where the Down that should have moved the
        // command palette's selection arrived as a *character*, appended
        // itself to the query, filtered the list to nothing and closed the
        // palette. Every key posted after a text write behaved that way, so
        // no typed value could ever be committed.
        //
        // Destroying and re-creating is what a fresh process does implicitly,
        // which is exactly why the same sequence worked one key per `neo ax
        // key` invocation and failed inside a single run.
        if let Some(previous) = self.keyboard.take() {
            previous.destroy();
        }
        let keyboard = self.keyboard()?;
        let text = keymap(symbols);
        let mut file = tempfile()?;
        let bytes = text.as_bytes();
        file.write_all(bytes).map_err(|e| AxError::NoVirtualInput {
            detail: format!("could not write the keymap: {e}"),
        })?;
        // libxkbcommon parses the mapping as a NUL-terminated string.
        file.write_all(&[0]).map_err(|e| AxError::NoVirtualInput {
            detail: format!("could not write the keymap: {e}"),
        })?;
        file.flush().map_err(|e| AxError::NoVirtualInput {
            detail: format!("could not write the keymap: {e}"),
        })?;
        let size = u32::try_from(bytes.len() + 1).unwrap_or(u32::MAX);
        keyboard.keymap(KEYMAP_TEXT_V1, file.as_fd(), size);
        self.flush();
        // The roundtrip above synchronises with the *compositor*, which is
        // not who has to understand these keycodes. The compositor forwards
        // `wl_keyboard.keymap` to the focused client, and that client — a
        // WebKitGTK window, here — mmaps and loads it on its own event-loop
        // turn. There is no protocol event to wait for: a client cannot be
        // synchronised with through the compositor, so the only correct
        // thing to wait is a moment.
        //
        // Without it, keys posted immediately after a *second* upload are
        // interpreted against the keymap the client still has. Measured on
        // degen-paint Studio: clearing a field and typing into it re-uploads
        // between the two, and the write landed nothing at all — which is
        // every overwrite of a field that already had a value.
        std::thread::sleep(KEYMAP_SETTLE);
        self.uploaded = symbols.to_vec();
        Ok(())
    }

    fn tap(&mut self, keyboard: &ZwpVirtualKeyboardV1, index: usize, mods: u32) {
        let code = u32::try_from(index).unwrap_or(0) + 9 - EVDEV_OFFSET;
        keyboard.modifiers(mods, 0, 0, 0);
        keyboard.key(now_ms(), code, 1);
        keyboard.key(now_ms(), code, 0);
        if mods != 0 {
            keyboard.modifiers(0, 0, 0, 0);
        }
        self.flush();
    }

    /// Post one key from the fixed set, with modifiers held for its
    /// duration.
    ///
    /// # Errors
    ///
    /// [`AxError::NoVirtualInput`] when the compositor has no virtual
    /// keyboard.
    pub(crate) fn press_key(&mut self, key: Key, modifiers: &[Modifier]) -> Result<(), AxError> {
        let symbol = keysym_of(key).to_owned();
        self.upload(std::slice::from_ref(&symbol))?;
        let keyboard = self.keyboard()?;
        self.tap(&keyboard, 0, mask_of(modifiers));
        Ok(())
    }

    /// Empty the focused field, without a chord.
    ///
    /// The write path needs a field cleared before it is typed into: a value
    /// that *appends* is the exact defect L1 fixed on the web path. This used
    /// to send ⌃A, and on this backend that silently did the opposite. The
    /// virtual-keyboard protocol takes a keymap this process uploads, and a
    /// one-symbol map defined as the *Unicode* codepoint `U0061` is a key
    /// that produces the text "a"; WebKitGTK under Wayland took the control
    /// modifier as a held state rather than a chord and inserted the literal
    /// character. Measured on degen-paint Studio: a field reading `artboard`
    /// became `artboarda`, and every write to a non-empty field failed with
    /// "the value did not appear" — which is every command palette, every
    /// filter and every form field that had a default in it.
    ///
    /// So the caret is driven instead, with two keys that are already in the
    /// navigator's fixed set and mean the same thing on every toolkit: Right
    /// to the end (a press past the end is a no-op), then Backspace for each
    /// character. No modifier is held, so nothing can be reinterpreted as
    /// text.
    ///
    /// # Errors
    ///
    /// [`AxError::NoVirtualInput`] when the compositor has no virtual
    /// keyboard.
    pub(crate) fn replace_text(
        &mut self,
        clear: usize,
        text: &str,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), AxError> {
        if stopped() {
            return Err(AxError::Stopped);
        }
        // A field is a field, not a document: the cap keeps a mis-measured
        // length from turning into thousands of key events.
        let clear = clear.min(MAX_CLEAR);
        let chars: Vec<char> = text.chars().collect();

        // One keycode per *distinct* symbol, not one per keystroke. That is
        // what lets a clear and the value that replaces it share a single
        // keymap: a field's text rarely has more than a few dozen distinct
        // characters, so the whole write fits in one upload and the client
        // never has to reload a layout mid-write.
        let mut symbols: Vec<String> = Vec::new();
        let index_of = |symbols: &mut Vec<String>, symbol: String| -> usize {
            match symbols.iter().position(|held| *held == symbol) {
                Some(index) => index,
                None => {
                    symbols.push(symbol);
                    symbols.len() - 1
                }
            }
        };
        let (right, backspace) = if clear > 0 {
            (
                Some(index_of(&mut symbols, "Right".to_owned())),
                Some(index_of(&mut symbols, "BackSpace".to_owned())),
            )
        } else {
            (None, None)
        };
        let keys: Vec<usize> = chars
            .iter()
            .map(|ch| index_of(&mut symbols, unicode_keysym(*ch)))
            .collect();
        if symbols.len() > KEYMAP_PAGE {
            // More distinct symbols than a keymap holds. Nothing sensible
            // types this into a form field, and a partial write is worse than
            // a refused one.
            return Err(AxError::Unsupported(
                "the value needs more distinct characters than one keymap holds",
            ));
        }

        self.upload(&symbols)?;
        let keyboard = self.keyboard()?;
        if let (Some(right), Some(backspace)) = (right, backspace) {
            // To the end first: a press past the end is a no-op, so this
            // needs no knowledge of where the caret actually was.
            for _ in 0..clear {
                self.tap(&keyboard, right, 0);
            }
            for _ in 0..clear {
                if stopped() {
                    return Err(AxError::Stopped);
                }
                self.tap(&keyboard, backspace, 0);
            }
        }
        for index in keys {
            if stopped() {
                return Err(AxError::Stopped);
            }
            self.tap(&keyboard, index, 0);
        }
        Ok(())
    }

    /// Type literal text into whatever has focus.
    ///
    /// # Errors
    ///
    /// [`AxError::NoVirtualInput`] when the compositor has no virtual
    /// keyboard; [`AxError::Stopped`] when the kill switch is thrown between
    /// pages, which is how a stop lands mid-typing.
    pub(crate) fn type_text(
        &mut self,
        text: &str,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), AxError> {
        if stopped() {
            return Err(AxError::Stopped);
        }
        let chars: Vec<char> = text.chars().collect();
        for page in chars.chunks(KEYMAP_PAGE) {
            if stopped() {
                return Err(AxError::Stopped);
            }
            let symbols: Vec<String> = page.iter().map(|c| unicode_keysym(*c)).collect();
            self.upload(&symbols)?;
            let keyboard = self.keyboard()?;
            for index in 0..page.len() {
                if stopped() {
                    return Err(AxError::Stopped);
                }
                self.tap(&keyboard, index, 0);
            }
        }
        Ok(())
    }
}

/// An anonymous file to hand the compositor.
///
/// `memfd_create` through `rustix`, because the workspace denies
/// `unsafe_code` and this would otherwise be a raw `libc` call.
fn tempfile() -> Result<std::fs::File, AxError> {
    let fd = rustix::fs::memfd_create("neo-ax-keymap", rustix::fs::MemfdFlags::CLOEXEC).map_err(
        |e| AxError::NoVirtualInput {
            detail: format!("could not create a keymap file: {e}"),
        },
    )?;
    Ok(std::fs::File::from(fd))
}

/// The X11 modifier mask for a chord.
fn mask_of(modifiers: &[Modifier]) -> u32 {
    modifiers.iter().fold(0, |mask, modifier| {
        mask | match modifier {
            Modifier::Shift => MOD_SHIFT,
            Modifier::Control => MOD_CONTROL,
            Modifier::Option => MOD_ALT,
            // There is no Command key. The navigator's key set is the
            // macOS one, and ⌘ is Super here: a chord that means "the
            // platform accelerator" lands on the platform's accelerator.
            Modifier::Command => MOD_SUPER,
        }
    })
}

/// The XKB keysym name for one of the fixed keys.
fn keysym_of(key: Key) -> &'static str {
    match key {
        Key::Return => "Return",
        Key::Escape => "Escape",
        Key::Tab => "Tab",
        Key::Space => "space",
        Key::Delete => "BackSpace",
        Key::Up => "Up",
        Key::Down => "Down",
        Key::Left => "Left",
        Key::Right => "Right",
    }
}

/// The XKB keysym name for one character.
fn unicode_keysym(ch: char) -> String {
    format!("U{:04X}", ch as u32)
}

/// An XKB keymap with one symbol per keycode, all at level 1.
fn keymap(symbols: &[String]) -> String {
    let mut out = String::with_capacity(256 + symbols.len() * 32);
    let last = 9 + symbols.len().max(1) - 1;
    out.push_str("xkb_keymap {\nxkb_keycodes \"neo\" {\n  minimum = 8;\n");
    out.push_str(&format!("  maximum = {last};\n"));
    for index in 0..symbols.len() {
        out.push_str(&format!("  <N{index}> = {};\n", 9 + index));
    }
    out.push_str("};\nxkb_types \"neo\" { include \"complete\" };\n");
    out.push_str("xkb_compatibility \"neo\" { include \"complete\" };\n");
    out.push_str("xkb_symbols \"neo\" {\n");
    for (index, symbol) in symbols.iter().enumerate() {
        out.push_str(&format!("  key <N{index}> {{ [ {symbol} ] }};\n"));
    }
    out.push_str("};\n};\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compositor parses this with libxkbcommon, so the shape is a
    /// contract: one keycode per symbol, starting at 9, and a `maximum`
    /// that covers them all. An off-by-one here is a keymap the compositor
    /// rejects and typing that silently does nothing.
    #[test]
    fn keymap_numbers_every_symbol_from_nine() {
        let text = keymap(&["U0041".to_owned(), "Return".to_owned()]);
        assert!(text.contains("  <N0> = 9;"), "{text}");
        assert!(text.contains("  <N1> = 10;"), "{text}");
        assert!(text.contains("  maximum = 10;"), "{text}");
        assert!(text.contains("key <N0> { [ U0041 ] };"), "{text}");
        assert!(text.contains("key <N1> { [ Return ] };"), "{text}");
    }

    /// Anything outside the Basic Multilingual Plane still has to arrive:
    /// an emoji in a typed string is a codepoint like any other.
    #[test]
    fn keysyms_cover_more_than_ascii() {
        assert_eq!(unicode_keysym('a'), "U0061");
        assert_eq!(unicode_keysym('é'), "U00E9");
        assert_eq!(unicode_keysym('中'), "U4E2D");
        assert_eq!(unicode_keysym('🙂'), "U1F642");
    }

    /// ⌘ has no Linux key. Mapping it to Super sends the chord to the
    /// compositor's own modifier rather than dropping it silently.
    #[test]
    fn command_becomes_super_and_masks_combine() {
        assert_eq!(mask_of(&[Modifier::Command]), MOD_SUPER);
        assert_eq!(
            mask_of(&[Modifier::Control, Modifier::Shift]),
            MOD_CONTROL | MOD_SHIFT
        );
        assert_eq!(mask_of(&[]), 0);
    }
}
