#![cfg(target_os = "macos")]

use crate::dbg_println;
use crate::updaters::Updater;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use system_configuration::network_reachability::{
    ReachabilityFlags, SCNetworkReachability, SchedulingError, SetCallbackError,
};
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

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

fn listen<F: Fn() + Sync, S: Future>(
    _notify_callback: F,
    shutdown: S,
) -> Result<S::Output, UpdaterError> {
    // let mut sc = SCNetworkReachability::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
    // sc.set_callback(|flags| {
    //     if has_internet_from_flags(flags) {
    //         notify_callback()
    //     }
    // })?;
    // unsafe { sc.schedule_with_runloop(&CFRunLoop::get_current(), kCFRunLoopDefaultMode)?; };

    Ok(TokioHandle::current().block_on(shutdown))
}

pub fn subscribe(updater: Updater) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let local_notify = Notify::new();
        let callback = || {
            dbg_println!("Network Listener: got network update!");
            if updater.update().is_err() {
                local_notify.notify_waiters();
            }
        };

        let shutdown = async {
            tokio::select! {
                _ = local_notify.notified()  => (),
                _ = updater.wait_shutdown() => ()
            }
        };

        let res = listen(callback, shutdown);
        updater.exit(res)
    })
}
