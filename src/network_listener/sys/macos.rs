#![cfg(target_os = "macos")]


use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use system_configuration::network_reachability::{ReachabilityFlags, SCNetworkReachability};
use tokio::sync::Notify;
use tokio::runtime::Handle as TokioHandle;
use tokio::task::JoinHandle;
use crate::dbg_println;
use crate::updaters::Updater;

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

pub fn subscribe(updater: Updater) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let local_notify = Notify::new();
        
        let mut sc = SCNetworkReachability::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
        let res = sc.set_callback(|flags| {
            dbg_println!("Network Listener: got network update!");
            if has_internet_from_flags(flags) && updater.update().is_err() {
                local_notify.notify_waiters();
            }
        });
        
        if let Err(e) = res {
            return updater.exit(Err(e));
        }
        
        TokioHandle::current().block_on(async { 
            tokio::select! {
                _ = local_notify.notified()  => (),
                _ = updater.wait_shutdown() => ()
            }
        });
    })
}