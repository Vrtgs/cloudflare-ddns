#![cfg(target_os = "macos")]

use crate::updaters::Updater;
use anyhow::Context;
use std::fmt::Debug;
use std::mem::MaybeUninit;
use std::net::Ipv6Addr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ptr::NonNull;
use system_configuration::network_reachability::{
    ReachabilityFlags, SCNetworkReachability, SchedulingError, SetCallbackError,
};
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

pub fn subscribe(updater: Updater) -> JoinHandle<()> {
    tokio::task::spawn(async move {
        let res = super::fallback_listen(&updater).await;
        updater.exit(res)
    })
}

fn get_ipv6_addr_sync_inner(path: &str) -> anyhow::Result<Option<Ipv6Addr>> {
    const IFA_F_TEMPORARY: u32 = 0x01;
    const GLOBAL_SCOPE: u8 = 0x00;

    let mut candidate = None;

    let contents = std::fs::read_to_string(path)?;
    for line in contents.lines() {
        // format: <32 hex chars addr> <ifindex> <prefix_len> <scope> <flags> <if_name>
        let mut fields = line.split_whitespace();
        let addr_hex = fields.next().context("missing addr")?;
        anyhow::ensure!(addr_hex.len() == 32, "invalid addr format");
        let _ifindex = fields.next().context("missing ifindex")?;
        let _prefix_len = fields.next().context("missing prefix_len")?;
        let scope = fields.next().context("missing scope")?;
        let flags = fields.next().context("missing flags")?;
        let _if_name = fields.next().context("missing if_name")?;

        let scope = u8::from_str_radix(scope, 16).context("could not parse scope")?;
        let flags = u32::from_str_radix(flags, 16).context("could not parse flags")?;

        if scope != GLOBAL_SCOPE {
            continue;
        }

        let ip_addr_bits =
            u128::from_str_radix(addr_hex, 16).context("could not parse ipv6 addr")?;

        let ip = Ipv6Addr::from_bits(ip_addr_bits);

        let valid_global = !ip.is_loopback()
            && !ip.is_unicast_link_local()
            && !ip.is_unique_local()
            && !ip.is_multicast();

        if !valid_global {
            continue;
        }

        if flags & IFA_F_TEMPORARY == 0 {
            candidate = Some(ip);
            break;
        }

        candidate = candidate.or(Some(ip));
    }

    Ok(candidate)
}

fn get_ipv6_addr_sync() -> anyhow::Result<Option<Ipv6Addr>> {
    let path = "/proc/net/if_inet6";
    get_ipv6_addr_sync_inner(path).with_context(|| format!("could not parse `{path}` file"))
}

pub async fn native_get_ipv6_addr() -> anyhow::Result<Option<Ipv6Addr>> {
    tokio::task::spawn_blocking(get_ipv6_addr_sync)
        .await
        .context("failed to spawn ipv6 native addr grab task")?
}
