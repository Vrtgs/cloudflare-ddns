use std::fs::OpenOptions;
use std::io::Write;
use std::num::NonZero;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use crate::updaters::Updater;
use crate::util::GLOBAL_TOKIO_RUNTIME;
use dbus::nonblock::{Proxy, SyncConnection};
use once_cell::sync::Lazy;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use futures::{StreamExt, TryStreamExt};
use tokio::net::UnixListener;
use tokio::sync::OnceCell as TokioOnceCell;
use tokio::task::JoinHandle;
use anyhow::Result;
use tempfile::TempPath;
use crate::util;

trait ArcExt<T> {
    fn leak(this: Self) -> &'static T;
}

impl<T> ArcExt<T> for Arc<T> {
    fn leak(this: Self) -> &'static T {
        // since we don't decrement this counter,
        // it will always be greater than 1 therefore the allocation is valid
        unsafe { &*Arc::into_raw(this) }
    }
}

#[derive(Debug, thiserror::Error)]
enum DbusError {
    #[error(transparent)]
    Init(#[from] &'static dbus::Error),
    #[error(transparent)]
    Connection(#[from] dbus::Error),
}

async fn check_network_status() -> Result<bool, DbusError> {
    static NETWORK_MANAGER: Lazy<Result<&SyncConnection, dbus::Error>> = Lazy::new(|| {
        let (resource, conn) = dbus_tokio::connection::new_session_sync()?;

        GLOBAL_TOKIO_RUNTIME.spawn(resource);

        Ok(Arc::leak(conn))
    });

    // Get a proxy to the NetworkManager object
    let proxy = Proxy::new(
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        Duration::from_secs(3),
        NETWORK_MANAGER.as_ref().copied()?,
    );

    // Call the Get method on the org.freedesktop.DBus.Properties interface
    let (connectivity,): (u32,) = proxy
        .method_call(
            "org.freedesktop.DBus.Properties",
            "Get",
            ("org.freedesktop.NetworkManager", "Connectivity"),
        )
        .await?;

    // value can be:
    //
    // 0: Unknown
    // 1: None
    // 2: Portal
    // 3: Limited
    // 4: Full
    Ok(connectivity >= 2)
}

pub async fn has_internet() -> bool {
    static SUPPORT_NETWORK_MANAGER: TokioOnceCell<bool> = TokioOnceCell::const_new();

    match SUPPORT_NETWORK_MANAGER.get_or_init(|| async { check_network_status().await.is_ok() }).await {
        true => match check_network_status().await {
            Ok(x) => x,
            Err(e) => {
                eprintln!("Unexpected error checking internet {e} switching to fallback");
                super::fallback_has_internet().await
            }
        }
        false => super::fallback_has_internet().await
    }
}

async fn place_dispatcher() -> Result<()> {
    let locations = include!("./dispatcher-locations")
        .map(Path::new)
        .map(|loc| loc.join(include_str!("./dispatcher-name")));

    let futures = locations.map(|location| async move {
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = location.parent() {
                if !location.try_exists()? && parent.try_exists()? {
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .mode(0o777)
                        .open(location)?.write_all(include_bytes!("./dispatcher"))?;
                }
            }
            Ok(())
        }).await?
    });

    let buffer = futures
        .len()
        .min(thread::available_parallelism().map_or(1, NonZero::get));

    futures::stream::iter(futures)
        .buffer_unordered(buffer)
        .try_collect()
        .await
}

async fn listen(updater: &Updater) -> Result<()> {
    place_dispatcher().await?;

    const SOCK: &str = include_str!("./socket-path");

    if util::try_exists(SOCK).await? {
        tokio::fs::remove_file(SOCK).await?;
    }
    let sock = TempPath::from_path(SOCK);
    let listener = UnixListener::bind(&sock)?;
    loop {
        let _ = listener.accept().await?;
        if updater.update().is_err() {
            return Ok(());
        }
    }
}

pub fn subscribe(updater: Updater) -> JoinHandle<()> {
    tokio::spawn(async move {
        let res = tokio::select! {
            res = listen(&updater) => res,
            _ = updater.wait_shutdown() => Ok(())
        };
        updater.exit(res)
    })
}
