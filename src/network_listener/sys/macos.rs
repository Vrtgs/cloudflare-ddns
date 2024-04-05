#![cfg(target_os = "macos")]


use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use core_foundation::runloop::CFRunLoop;
use core_foundation::string::CFString;
use core_foundation_sys::runloop::kCFRunLoopDefaultMode;
use core_foundation_sys::string::CFStringRef;
use system_configuration::network_reachability::{ReachabilityFlags, SchedulingError, SCNetworkReachability, SetCallbackError};
use tokio::sync::Notify;
use tokio::runtime::Handle as TokioHandle;
use tokio::task::JoinHandle;
use crate::dbg_println;
use crate::updaters::Updater;

#[derive(thiserror::Error, Debug)]
pub enum UpdaterError {
    #[error("Couldn't set the callback to network events: {0}")]
    Callback(#[from] SetCallbackError),

    #[error("Couldn't Schedule callback execution with CFRunloop: {0}")]
    Runloop(#[from] SchedulingError),
}

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

fn listen<F: Fn() + Sync, S: Future>(notify_callback: F, shutdown: S) -> Result<S::Output, UpdaterError> {
    let mut sc = SCNetworkReachability::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
    sc.set_callback(|flags| {
        if has_internet_from_flags(flags) {
            notify_callback()
        }
    })?;

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