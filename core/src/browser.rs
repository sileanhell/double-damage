//! Opening a sign-in page in the user's browser.

/// Hands `url` to the default browser. Only `http(s)` URLs are accepted.
pub fn open(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("refusing to open {url:?}"));
    }
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::{HSTRING, PCWSTR},
    };
    let verb = HSTRING::from("open");
    let target = HSTRING::from(url);
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe { ShellExecuteW(None, &verb, &target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // ShellExecute reports success with a value above 32.
    if result.0 as usize > 32 { Ok(()) } else { Err(format!("can't open the browser (error {})", result.0 as usize)) }
}
