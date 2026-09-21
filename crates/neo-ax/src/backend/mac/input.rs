//! HID-level input: `CGEvent` keys, typed text and the kill switch.
//!
//! Only reached when the AX path cannot do the job (`type_text` and `key`
//! have no AX equivalent). Three rules hold everywhere in this module:
//!
//! * **Secure input wins.** If another process has secure event input on, we
//!   refuse rather than drop keystrokes into a password field we cannot see.
//! * **Modifiers are RAII.** A held modifier's `Drop` posts its key-up, so a
//!   panic between key-down and key-up cannot leave ⌘ stuck.
//! * **The kill switch is lock-free.** An `AtomicBool` is checked between
//!   typed chunks, so a stop lands within one chunk (~10 ms), mid-typing.

#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};

use crate::error::AxError;
use crate::types::{Key, Modifier};

/// UTF-16 units per synthetic keyboard event.
const CHUNK_UNITS: usize = 20;

// SAFETY: the declaration matches HIToolbox's
// `extern Boolean IsSecureEventInputEnabled(void);` exactly — no arguments,
// a C `bool`-compatible return, and no ownership transfer — so calls through
// it cannot violate the ABI.
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// True while any process holds secure event input (a password field is
    /// focused somewhere). Declared by hand: no `objc2` crate covers Carbon's
    /// HIToolbox.
    fn IsSecureEventInputEnabled() -> bool;
}

/// Whether some process has secure event input enabled.
#[must_use]
pub(crate) fn secure_input_enabled() -> bool {
    // SAFETY: a nullary Carbon predicate with no arguments, no out-params and
    // no threading requirement; it only reads global HIToolbox state.
    unsafe { IsSecureEventInputEnabled() }
}

/// Virtual key codes for the fixed, chord-free key set.
const fn keycode(key: Key) -> u16 {
    match key {
        Key::Return => 0x24,
        Key::Tab => 0x30,
        Key::Space => 0x31,
        Key::Delete => 0x33,
        Key::Escape => 0x35,
        Key::Left => 0x7B,
        Key::Right => 0x7C,
        Key::Down => 0x7D,
        Key::Up => 0x7E,
    }
}

/// Virtual key codes of the modifier keys themselves.
const MODIFIER_KEYCODES: [u16; 5] = [
    0x38, // shift
    0x3B, // control
    0x3A, // option
    0x37, // command
    0x3D, // right option
];

const fn modifier_keycode(m: Modifier) -> u16 {
    match m {
        Modifier::Shift => 0x38,
        Modifier::Control => 0x3B,
        Modifier::Option => 0x3A,
        Modifier::Command => 0x37,
    }
}

const fn modifier_flag(m: Modifier) -> CGEventFlags {
    match m {
        Modifier::Shift => CGEventFlags::MaskShift,
        Modifier::Control => CGEventFlags::MaskControl,
        Modifier::Option => CGEventFlags::MaskAlternate,
        Modifier::Command => CGEventFlags::MaskCommand,
    }
}

/// Where a synthetic event is delivered.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Target {
    /// Straight to one process. Preferred: it cannot land in another app.
    Pid(i32),
    /// The HID tap, i.e. wherever focus is. Only after a frontmost check.
    Global,
}

fn post(event: &CGEvent, target: Target) {
    match target {
        Target::Pid(pid) => CGEvent::post_to_pid(pid, Some(event)),
        Target::Global => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event)),
    }
}

fn key_event(
    code: u16,
    down: bool,
    flags: CGEventFlags,
) -> Option<objc2_core_foundation::CFRetained<CGEvent>> {
    let event = CGEvent::new_keyboard_event(None, code, down)?;
    if !flags.is_empty() {
        CGEvent::set_flags(Some(&event), flags);
    }
    Some(event)
}

/// Modifiers held down for the duration of a key press.
///
/// Dropping the guard posts every key-up, whether the press succeeded, failed
/// or panicked.
pub(crate) struct HeldModifiers {
    codes: Vec<u16>,
    target: Target,
}

impl HeldModifiers {
    /// Press each modifier down and keep them down until the guard is dropped.
    pub(crate) fn press(modifiers: &[Modifier], target: Target) -> Self {
        let mut held = Self {
            codes: Vec::with_capacity(modifiers.len()),
            target,
        };
        let mut flags = CGEventFlags::empty();
        for m in modifiers {
            flags |= modifier_flag(*m);
            let code = modifier_keycode(*m);
            if let Some(event) = key_event(code, true, flags) {
                post(&event, target);
                held.codes.push(code);
            }
        }
        held
    }

    /// The flag mask the held modifiers add to the next event.
    pub(crate) fn flags(&self, modifiers: &[Modifier]) -> CGEventFlags {
        modifiers
            .iter()
            .fold(CGEventFlags::empty(), |acc, m| acc | modifier_flag(*m))
    }
}

impl Drop for HeldModifiers {
    fn drop(&mut self) {
        for code in self.codes.iter().rev() {
            if let Some(event) = key_event(*code, false, CGEventFlags::empty()) {
                post(&event, self.target);
            }
        }
    }
}

/// Post key-ups for every modifier, whatever we think is held.
///
/// The kill switch calls this: it must leave no modifier down even if our own
/// bookkeeping is wrong or a guard was leaked.
pub(crate) fn release_all_modifiers() {
    for code in MODIFIER_KEYCODES {
        if let Some(event) = key_event(code, false, CGEventFlags::empty()) {
            post(&event, Target::Global);
        }
    }
}

/// Post one key, with modifiers held only for its duration.
pub(crate) fn press_key(key: Key, modifiers: &[Modifier], target: Target) -> Result<(), AxError> {
    if secure_input_enabled() {
        return Err(AxError::SecureInput);
    }
    let held = HeldModifiers::press(modifiers, target);
    let flags = held.flags(modifiers);
    let code = keycode(key);
    let down = key_event(code, true, flags).ok_or(AxError::Unsupported(
        "the system refused to create a key event",
    ))?;
    post(&down, target);
    let up = key_event(code, false, flags).ok_or(AxError::Unsupported(
        "the system refused to create a key event",
    ))?;
    post(&up, target);
    drop(held);
    Ok(())
}

/// Split text into UTF-16 chunks of at most 20 units, never splitting a
/// surrogate pair.
pub(crate) fn utf16_chunks(text: &str) -> Vec<Vec<u16>> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < units.len() {
        let mut end = (start + CHUNK_UNITS).min(units.len());
        // A high surrogate at the last position owns the next unit.
        if end < units.len() && (0xD800..0xDC00).contains(&units[end - 1]) {
            end += 1;
        }
        chunks.push(units[start..end].to_vec());
        start = end;
    }
    chunks
}

/// Type literal text into whatever has focus.
///
/// `kill` is checked between chunks, so the kill switch stops typing within
/// one chunk instead of after the whole string.
pub(crate) fn type_text(text: &str, target: Target, kill: &Arc<AtomicBool>) -> Result<(), AxError> {
    // The kill switch is checked before anything else, including secure
    // input. A stopped run must report that it was stopped, whatever else is
    // true of the machine at that instant — and secure input is genuinely
    // volatile, because a password field anywhere (a Keychain prompt, the
    // lock screen) turns it on system-wide for every process.
    if kill.load(Ordering::Relaxed) {
        release_all_modifiers();
        return Err(AxError::Stopped);
    }
    if secure_input_enabled() {
        return Err(AxError::SecureInput);
    }
    for chunk in utf16_chunks(text) {
        if kill.load(Ordering::Relaxed) {
            release_all_modifiers();
            return Err(AxError::Stopped);
        }
        for down in [true, false] {
            let Some(event) = CGEvent::new_keyboard_event(None, 0, down) else {
                return Err(AxError::Unsupported(
                    "the system refused to create a key event",
                ));
            };
            let len = u64::try_from(chunk.len()).unwrap_or(0);
            // SAFETY: `chunk` is a live `Vec<u16>` for the whole call and
            // `len` is exactly its length, which is the binding's contract
            // (`unicode_string` valid for `string_length` units).
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(&event), len, chunk.as_ptr());
            }
            post(&event, target);
        }
    }
    Ok(())
}

/// Scroll by whole lines over whatever is under focus.
pub(crate) fn scroll_wheel(lines: i32, target: Target) -> Result<(), AxError> {
    let event = CGEvent::new_scroll_wheel_event2(
        None,
        objc2_core_graphics::CGScrollEventUnit::Line,
        1,
        lines,
        0,
        0,
    )
    .ok_or(AxError::Unsupported(
        "the system refused to create a scroll event",
    ))?;
    post(&event, target);
    Ok(())
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests opt out of unwrap_used (05 §2)")]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_at_most_twenty_utf16_units() {
        let text = "a".repeat(101);
        let chunks = utf16_chunks(&text);
        assert_eq!(chunks.len(), 6);
        assert!(chunks.iter().all(|c| c.len() <= CHUNK_UNITS));
        let rebuilt: Vec<u16> = chunks.concat();
        assert_eq!(rebuilt, text.encode_utf16().collect::<Vec<_>>());
    }

    #[test]
    fn a_surrogate_pair_is_never_split_across_chunks() {
        // 19 ASCII chars then an emoji: the pair would straddle unit 20.
        let text = format!("{}🙂🙂", "a".repeat(19));
        let chunks = utf16_chunks(&text);
        assert_eq!(
            chunks[0].len(),
            21,
            "the chunk stretches to keep the pair whole"
        );
        for chunk in &chunks {
            assert!(
                String::from_utf16(chunk).is_ok(),
                "every chunk must be valid UTF-16 on its own"
            );
        }
        assert_eq!(String::from_utf16(&chunks.concat()).unwrap(), text);
    }

    #[test]
    fn zwj_sequences_survive_chunking() {
        let family = "👨‍👩‍👧‍👦";
        let text = family.repeat(4);
        let chunks = utf16_chunks(&text);
        for chunk in &chunks {
            assert!(String::from_utf16(chunk).is_ok());
        }
        assert_eq!(String::from_utf16(&chunks.concat()).unwrap(), text);
    }

    #[test]
    fn empty_text_produces_no_events() {
        assert!(utf16_chunks("").is_empty());
    }

    #[test]
    fn every_key_in_the_fixed_set_has_a_distinct_code() {
        let keys = [
            Key::Return,
            Key::Escape,
            Key::Tab,
            Key::Space,
            Key::Delete,
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
        ];
        let mut codes: Vec<u16> = keys.iter().map(|k| keycode(*k)).collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), before);
    }

    #[test]
    fn modifier_flags_combine_without_overlap() {
        let all = [
            Modifier::Shift,
            Modifier::Control,
            Modifier::Option,
            Modifier::Command,
        ];
        let combined = all
            .iter()
            .fold(CGEventFlags::empty(), |acc, m| acc | modifier_flag(*m));
        assert_eq!(combined.bits().count_ones(), 4);
    }

    /// A stop that arrives before the first chunk must post nothing and say
    /// why. Posting is a no-op without the grant, so this exercises the
    /// control flow, not the HID layer.
    #[test]
    fn the_kill_switch_stops_typing_immediately() {
        let kill = Arc::new(AtomicBool::new(true));
        let err = type_text("hello", Target::Pid(-1), &kill).unwrap_err();
        assert!(matches!(err, AxError::Stopped));
    }
}
