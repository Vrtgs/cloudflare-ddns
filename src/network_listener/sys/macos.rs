#![cfg(target_os = "macos")]


use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use system_configuration::network_reachability::{ReachabilityFlags, SCNetworkReachability};
use tokio::sync::Notify;
use tokio::runtime::Handle as TokioHandle;
use crate::dbg_println;
use crate::updaters::UpdatersManager;

#[must_use = "its useless to check if we have internet if you dont use it"]
pub async fn has_internet() -> bool {
    let sc = SCNetworkReachability::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
    sc.reachability().map(has_internet_from_flags).unwrap_or(false)
}

fn has_internet_from_flags(flags: ReachabilityFlags) -> bool {
    if !flags.contains(ReachabilityFlags::REACHABLE) {
        return false;
    }
    if !flags.contains(ReachabilityFlags::CONNECTION_REQUIRED)
        || ((flags.contains(ReachabilityFlags::CONNECTION_ON_DEMAND)
        || flags.contains(ReachabilityFlags::CONNECTION_ON_TRAFFIC))
        && !flags.contains(ReachabilityFlags::INTERVENTION_REQUIRED))
    {
        return true;
    }
    if flags == ReachabilityFlags::IS_WWAN {
        return true;
    }
    false
}

pub fn subscribe(updaters_manager: &mut UpdatersManager) {
    let (updater, jh_entry) = updaters_manager.add_updater("network-listener");
    jh_entry.insert(tokio::task::spawn_blocking(move || {
        let local_notif = Notify::new();
        
        let mut sc = SCNetworkReachability::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
        let res = sc.set_callback(|flags| {
            dbg_println!("Network Listener: got network update!");
            if has_internet_from_flags(flags) && updater.update().is_err() {
                local_notif.notify_waiters();
            }
        });
        
        if let Err(e) = res {
            return updater.exit(Err(e));
        }
        
        TokioHandle::current().block_on(async { 
            tokio::select! {
                _ = local_notif.notified()  => (),
                _ = updater.wait_shutdown() => ()
            }
        });
    }));

}