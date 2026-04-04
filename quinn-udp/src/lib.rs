#![warn(unreachable_pub)]
#![warn(clippy::use_self)]
#[cfg(any(unix, windows))]
mod cmsg;
#[cfg(unix)]
#[path = "unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "windows.rs"]
mod imp;
#[cfg(not(any(wasm_browser, unix, windows)))]
#[path = "fallback.rs"]
mod imp;
#[allow(unused_imports, unused_macros)]
mod log {}
#[cfg(test)]
mod tests {}
