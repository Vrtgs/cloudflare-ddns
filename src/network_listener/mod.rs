#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(target_os = "linux", path = "linux/mod.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
mod sys_common;

use crate::updaters::UpdatersManager;
use std::convert::Infallible;
use std::net::IpAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::try_join;
use ip_macro::ip;

#[must_use = "its useless to check if we have internet if you dont use it"]
#[inline(always)]
pub async fn has_internet() -> bool {
    sys_common::has_internet().await
}

#[allow(dead_code)]
async fn fallback_has_internet() -> bool {
    macro_rules! test_internet_from {
        ($([$name: ident, $ip: expr])*) => {{
            const IPS: [IpAddr; 8] = [$($ip),*];
            
            struct HasInternet;
            struct NoInternet;
        
            let [$($name),*] = IPS.map(|ip| async move {
                match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((ip, 53))).await {
                    Ok(Ok(_)) => Err(HasInternet),
                    _ => Ok(NoInternet),
                }
            });
            
            async move { matches!(try_join!($($name),*), Err(HasInternet)) }
        }};
    }

    test_internet_from!(
        // cloudflare
        [cloudflare, ip!("1.1.1.1")]
        [cloudflare_alt, ip!("1.0.0.1")]
        [cloudflare_v6, ip!("2606:4700:4700::1111")]
        [cloudflare_alt_v6, ip!("2606:4700:4700::1001")]
        
        // google
        [google, ip!("8.8.8.8")]
        [google_alt, ip!("8.8.4.4")]
        [google_v6, ip!("2001:4860:4860::8888")]
        [google_alt_v6, ip!("2001:4860:4860::8844")]
    ).await
}

pub fn subscribe(updaters_manager: &mut UpdatersManager) -> Result<(), Infallible> {
    let (updater, jh_entry) = updaters_manager.add_updater("network-listener");
    jh_entry.insert(sys_common::subscribe(updater));
    Ok(())
}
