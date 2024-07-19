#![cfg(target_os = "macos")]

use std::convert::Infallible;
use crate::dbg_println;
use crate::updaters::Updater;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use system_configuration::network_reachability::{
    ReachabilityFlags, SCNetworkReachability, SchedulingError, SetCallbackError,
};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use crate::util::new_skip_interval_after;

#[derive(thiserror::Error, Debug)]
pub enum UpdaterError {
    #[error("Couldn't set the callback to network events: {0}")]
    Callback(#[from] SetCallbackError),

    #[error("Couldn't Schedule callback execution with CFRunloop: {0}")]
    Runloop(#[from] SchedulingError),
}

pub async fn has_internet() -> bool {
    let sc = SCNetworkReachability::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
    sc.reachability().map_or(false, has_internet_from_flags)
}

fn has_internet_from_flags(flags: ReachabilityFlags) -> bool {
    flags.contains(ReachabilityFlags::REACHABLE)
        && (!flags.contains(ReachabilityFlags::CONNECTION_REQUIRED)
            || ((flags.contains(ReachabilityFlags::CONNECTION_ON_DEMAND)
                || flags.contains(ReachabilityFlags::CONNECTION_ON_TRAFFIC))
                && !flags.contains(ReachabilityFlags::INTERVENTION_REQUIRED))
            || flags.contains(ReachabilityFlags::IS_WWAN))
}

pub fn subscribe(updater: Updater) -> JoinHandle<()> {
    tokio::task::spawn(async move {
        let local_notify = Notify::new();
        let callback = || {
            dbg_println!("Network Listener: got network update!");
            if updater.update().is_err() {
                local_notify.notify_waiters();
            }
        };
        
        let listen_loop = async move {
            let mut timer = new_skip_interval_after(Duration::from_secs(30));
            let mut last: bool = has_internet().await;
            loop {
                timer.tick().await;
                let new = has_internet().await;
                if last != new { 
                    last = new;
                    callback()
                }
            }
        };
        
        tokio::select! {
            never = listen_loop => {
                let never: Infallible = never;
                match never {}
            },
            _ = local_notify.notified()  => (),
            _ = updater.wait_shutdown() => ()
        }

        updater.exit(Ok::<_, Infallible>(()))
    })
}
