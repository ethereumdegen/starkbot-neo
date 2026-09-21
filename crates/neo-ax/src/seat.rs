//! Whether a run may take the seat — the keyboard and the focused window.
//!
//! It may not, by default, and that default is the point.
//!
//! A run shares the machine with the person sitting at it. Three things are
//! theirs and cannot be borrowed without them noticing:
//!
//! - the **pointer**, which is not reachable from this crate at all any more;
//!   the Wayland protocol for it is not bound (`backend::atspi::input`);
//! - the **focused window**, because raising one takes their typing target,
//!   and a compositor that warps on focus — Hyprland's default — drags their
//!   cursor across the screen with it;
//! - the **keyboard**, because a synthetic key goes to whatever is focused,
//!   which is to say into whatever they were in the middle of writing.
//!
//! So none of them are taken. What is left is plenty: AT-SPI `DoAction`
//! presses a control in place, without focus and without a pointer, and an
//! application that declares a control channel in its pack can be told to do
//! anything its own front end can do. Reading — the element table the
//! navigator observes — never needed the seat in the first place.
//!
//! An action that genuinely cannot be done this way is **refused by name**
//! rather than done anyway. A refusal the caller can read and route around is
//! worth more than a keystroke landing in somebody's document.

/// Set to `1` to let a run take the keyboard and raise windows.
///
/// For a machine nobody is using — CI, a dedicated box — where the older
/// behaviour is what is wanted. Never the default: the cost of guessing wrong
/// is paid by a person, not by a test.
pub const TAKE_SEAT_ENV: &str = "NEO_TAKE_SEAT";

/// May this process take the keyboard and the focused window?
#[must_use]
pub fn may_take_seat() -> bool {
    permits(std::env::var(TAKE_SEAT_ENV).ok().as_deref())
}

/// The rule itself, over the variable's value — separated from reading the
/// environment so it can be tested without mutating the process (which Rust
/// 2024 makes `unsafe`, and this workspace forbids `unsafe_code`).
fn permits(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        value == "1" || value.eq_ignore_ascii_case("true")
    })
}

/// The sentence a refused action carries, so every one of them reads the same.
pub const REFUSED: &str = "this would need the keyboard and the focused window, which a run does not take while \
     somebody is using the machine — drive it through the application's control channel, or \
     set NEO_TAKE_SEAT=1 on a machine nobody is at";

#[cfg(test)]
mod tests {
    use super::permits;

    /// The default is the whole point: anything other than an explicit
    /// opt-in — unset above all — must not be read as permission to take
    /// somebody's keyboard and window.
    #[test]
    fn nothing_but_an_explicit_opt_in_takes_the_seat() {
        assert!(!permits(None), "unset must never permit");
        for refused in ["", "0", "no", "yes", "please", "false", "2"] {
            assert!(!permits(Some(refused)), "{refused:?} must not permit");
        }
        for allowed in ["1", "true", "TRUE", " 1 ", "True"] {
            assert!(permits(Some(allowed)), "{allowed:?} must permit");
        }
    }
}
