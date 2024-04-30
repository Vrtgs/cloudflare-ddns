#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
mod sys_common;

use std::convert::Infallible;
use crate::updaters::UpdatersManager;


pub use sys_common::has_internet;
pub fn subscribe(updaters_manager: &mut UpdatersManager) -> Result<(), Infallible> {
    let (updater, jh_entry) = updaters_manager.add_updater("network-listener");
    jh_entry.insert(sys_common::subscribe(updater));

    Ok(())
}
