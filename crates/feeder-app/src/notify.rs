//! macOS notifications for new updates. Inside the app bundle they go through UserNotifications
//! (`notify.m`), so they carry Feeder's name and icon and a click opens the project. A `cargo run`
//! build has no bundle, so it falls back to `osascript`, which macOS shows as Script Editor's.

use std::ffi::{CString, c_char};

unsafe extern "C" {
    fn feeder_notify_start();
    fn feeder_notify(title: *const c_char, body: *const c_char, url: *const c_char) -> bool;
}

/// Installs the click handler, so a notification from an earlier run still opens its project.
pub fn start() {
    // SAFETY: called once from Rune's `on_window`, on the AppKit main thread.
    unsafe { feeder_notify_start() }
}

/// Shows `body` under `title`; clicking it opens `link` (a `feeder://` URL).
pub fn post(title: &str, body: &str, link: &str) {
    let preview = crate::feed::first_line(body, 140);
    let c = |s: &str| CString::new(s.replace('\0', "")).unwrap_or_default();
    let (title_c, body_c, link_c) = (c(title), c(&preview), c(link));
    // SAFETY: the three pointers are NUL-terminated and outlive the call; notify.m copies them
    // into NSStrings before it returns.
    let native = unsafe { feeder_notify(title_c.as_ptr(), body_c.as_ptr(), link_c.as_ptr()) };
    if !native {
        let title = title.to_string();
        std::thread::spawn(move || {
            let _ = std::process::Command::new("/usr/bin/osascript")
                .args([
                    "-e",
                    "on run argv\ndisplay notification (item 1 of argv) with title (item 2 of argv)\nend run",
                    "--",
                    &preview,
                    &title,
                ])
                .output();
        });
    }
}
