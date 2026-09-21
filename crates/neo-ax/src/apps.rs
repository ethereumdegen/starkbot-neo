//! Running applications, over `NSRunningApplication`.
//!
//! # Why not `NSWorkspace.runningApplications`
//!
//! `NSWorkspace`'s list and its `frontmostApplication` are maintained from
//! **the main thread's run loop**. Neo's AX work happens on the actor thread
//! and the host process may never pump a main run loop at all, so those two
//! properties go stale the moment an app launches, quits or comes forward —
//! measured here: an app launched two seconds earlier is still missing, and
//! `frontmostApplication` still names the previous app after an activation
//! that demonstrably succeeded.
//!
//! A freshly constructed `NSRunningApplication` does not have that problem:
//! `runningApplicationWithProcessIdentifier` asks LaunchServices there and
//! then, so `isActive` / `ownsMenuBar` are live. This module therefore
//! enumerates pids with `proc_listpids` (≈0.2 ms for 600 processes) and
//! builds a fresh `NSRunningApplication` per pid (≈8 ms for the ~50 that are
//! apps), which is the only combination that is both live and cheap.
//!
//! The only `unsafe` here is the pair of `proc_listpids` calls.

#![allow(unsafe_code)]

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;

use crate::types::AppInfo;

/// Only regular, dock-visible apps. Agents and UI-element processes are
/// never automation targets and would swamp the list.
const POLICY_REGULAR: isize = 0;

/// `PROC_ALL_PIDS`, from `<libproc.h>`. Not in the `libc` crate.
const PROC_ALL_PIDS: u32 = 1;

/// Every pid on the system, in kernel order.
fn all_pids() -> Vec<i32> {
    // SAFETY: the sizing call passes a null buffer with length 0, which is
    // how `proc_listpids` is documented to report the required size.
    let bytes = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Vec::new();
    }
    // Slack: processes can appear between the sizing call and the fill.
    let capacity = (bytes as usize) / size_of::<i32>() + 64;
    let mut buf = vec![0i32; capacity];
    let size = i32::try_from(capacity * size_of::<i32>()).unwrap_or(i32::MAX);
    // SAFETY: `buf` owns `capacity * 4` bytes and `size` says exactly that,
    // so the kernel cannot write past the end. It returns the bytes written.
    let written = unsafe { libc::proc_listpids(PROC_ALL_PIDS, 0, buf.as_mut_ptr().cast(), size) };
    if written <= 0 {
        return Vec::new();
    }
    buf.truncate((written as usize) / size_of::<i32>());
    buf.retain(|pid| *pid > 0);
    buf
}

fn info_of(app: &NSRunningApplication) -> AppInfo {
    AppInfo {
        name: app
            .localizedName()
            .map(|n| (*n).to_string())
            .unwrap_or_default(),
        bundle_id: app.bundleIdentifier().map(|b| (*b).to_string()),
        pid: app.processIdentifier(),
        // Owning the menu bar *is* being frontmost, and unlike
        // `NSWorkspace.frontmostApplication` it is answered live.
        frontmost: app.ownsMenuBar(),
    }
}

fn regular_app(pid: i32) -> Option<objc2::rc::Retained<NSRunningApplication>> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    if app.isTerminated() || app.activationPolicy().0 != POLICY_REGULAR {
        return None;
    }
    Some(app)
}

/// Every regular running application, read live.
pub(crate) fn running() -> Vec<AppInfo> {
    all_pids()
        .into_iter()
        .filter_map(|pid| regular_app(pid).map(|a| info_of(&a)))
        .collect()
}

/// The app that owns the menu bar right now.
pub(crate) fn frontmost() -> Option<AppInfo> {
    running().into_iter().find(|a| a.frontmost)
}

/// Whether this pid owns the menu bar. One live LaunchServices query.
pub(crate) fn is_frontmost(pid: i32) -> bool {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .is_some_and(|a| a.ownsMenuBar())
}

/// Fresh information about one pid, or `None` when it is gone or is not a
/// regular app.
pub(crate) fn by_pid(pid: i32) -> Option<AppInfo> {
    regular_app(pid).map(|a| info_of(&a))
}

/// Bring an app to the front. Returns whether the request was accepted; the
/// caller still verifies frontmost-ness before posting any HID event.
pub(crate) fn activate(pid: i32) -> bool {
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return false;
    };
    app.unhide();
    app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
}

/// Launch an app by its user-visible name.
///
/// The caller has already cleared the name against the deny list. Returns
/// whether the launch request was accepted; the app shows up in [`running`]
/// a moment later.
///
/// The name goes through LaunchServices before it is launched, because
/// `launchApplication:` matches the bundle's *file* name: Apple ships Numbers
/// as `Numbers Creator Studio.app`, so launching "Numbers" — the name its own
/// menu bar, `CFBundleName` and every model answer use — failed outright until
/// the lookup was added. `fullPathForApplication:` is the resolution `open -a`
/// performs, and its answer always launches.
#[expect(
    deprecated,
    reason = "the replacement, -openApplicationAtURL:configuration:completionHandler:, needs a \
              completion block and a run loop to deliver it on; the AX actor thread pumps neither"
)]
pub(crate) fn launch(name: &str) -> bool {
    let workspace = NSWorkspace::sharedWorkspace();
    let name = NSString::from_str(name);
    let resolved = workspace.fullPathForApplication(&name);
    workspace.launchApplication(resolved.as_deref().unwrap_or(&name))
}

/// Launch an app by bundle id.
///
/// The bundle id is the only selector that cannot be wrong — two apps can
/// share a name, and a renamed bundle answers to neither — so the navigator
/// hands one over whenever it has one, and an app that is not running yet has
/// to be startable from it. LaunchServices maps the id to the installed
/// bundle; the path it returns is launched exactly like a resolved name.
#[expect(
    deprecated,
    reason = "same as `launch`: the modern API needs a completion block and a run loop"
)]
pub(crate) fn launch_bundle_id(bundle_id: &str) -> bool {
    let workspace = NSWorkspace::sharedWorkspace();
    let Some(url) = workspace.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))
    else {
        return false;
    };
    let Some(path) = url.path() else {
        return false;
    };
    workspace.launchApplication(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Listing apps needs no permission and no running target: on any Mac,
    /// Finder is a regular running app.
    #[test]
    fn running_apps_are_listed_with_pids_and_one_menu_bar_owner() {
        let apps = running();
        assert!(
            !apps.is_empty(),
            "at least one regular app is always running"
        );
        assert!(apps.iter().all(|a| a.pid > 0));
        assert!(apps.iter().all(|a| !a.name.is_empty()));
        assert!(
            apps.iter().filter(|a| a.frontmost).count() <= 1,
            "only one app can own the menu bar"
        );
    }

    #[test]
    fn enumeration_sees_every_pid_including_this_process() {
        let pids = all_pids();
        assert!(pids.len() > 10, "a Mac always runs more than ten processes");
        let own = std::process::id().cast_signed();
        assert!(pids.contains(&own), "our own pid must be in the list");
    }

    #[test]
    fn by_pid_agrees_with_the_listing() {
        let apps = running();
        let Some(first) = apps.first() else {
            return;
        };
        let Some(again) = by_pid(first.pid) else {
            return; // the app quit between the two calls
        };
        assert_eq!(again.pid, first.pid);
        assert_eq!(again.bundle_id, first.bundle_id);
        assert_eq!(again.frontmost, is_frontmost(first.pid));
    }

    #[test]
    fn a_dead_pid_has_no_info_and_cannot_be_activated_or_be_frontmost() {
        assert!(by_pid(-1).is_none());
        assert!(!activate(-1));
        assert!(!is_frontmost(-1));
    }
}
