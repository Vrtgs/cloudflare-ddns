#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "linux", path = "linux/mod.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
mod sys_common;

use crate::updaters::UpdatersManager;
use std::convert::Infallible;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::try_join;

#[must_use = "its useless to check if we have internet if you dont use it"]
#[inline(always)]
pub async fn has_internet() -> bool {
    sys_common::has_internet().await
}

#[allow(unused)]
async fn fallback_has_internet() -> bool {
    macro_rules! ip {
        ($($lit: literal),+) => {
            std::net::Ipv4Addr::new($($lit),*)
        };
    }

    struct HasInternet;
    struct NoInternet;

    let [fut1, fut2] = [ip!(1, 1, 1, 1), ip!(8, 8, 8, 8)].map(|ip| async move {
        match tokio::time::timeout(Duration::from_secs(3), TcpStream::connect((ip, 53))).await {
            Ok(Ok(_)) => Err(HasInternet),
            _ => Ok(NoInternet),
        }
    });

    matches!(try_join!(fut1, fut2), Err(HasInternet))
}

pub fn subscribe(updaters_manager: &mut UpdatersManager) -> Result<(), Infallible> {
    let (updater, jh_entry) = updaters_manager.add_updater("network-listener");
    jh_entry.insert(sys_common::subscribe(updater));
    Ok(())
}
