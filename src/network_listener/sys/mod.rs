#[cfg(windows)]
mod windows;

use std::convert::Infallible;
#[cfg(windows)]
use windows as sys;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as sys;

pub use sys::has_internet;

use crate::updaters::UpdatersManager;
pub fn subscribe(updaters_manager: &mut UpdatersManager) -> Result<(), Infallible> {
    let (updater, jh_entry) = updaters_manager.add_updater("network-listener");
    jh_entry.insert(sys::subscribe(updater));

    Ok(())
}
