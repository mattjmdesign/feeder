use std::ffi::{CString, c_char};

unsafe extern "C" {
    fn feeder_notify_start();
    fn feeder_notify(title: *const c_char, body: *const c_char, url: *const c_char) -> bool;
    fn feeder_app_is_active() -> bool;
}

pub fn start() {
    unsafe { feeder_notify_start() }
}

pub fn app_is_active() -> bool {
    unsafe { feeder_app_is_active() }
}

pub fn post(title: &str, body: &str, link: &str) {
    let preview = feeder_core::markdown_excerpt(body, 140);
    let preview = if preview.is_empty() { "New update".to_string() } else { preview };
    let c = |s: &str| CString::new(s.replace('\0', "")).unwrap_or_default();
    let (title_c, body_c, link_c) = (c(title), c(&preview), c(link));
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
