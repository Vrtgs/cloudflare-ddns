#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{subscribe, has_internet};


#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{subscribe, has_internet};