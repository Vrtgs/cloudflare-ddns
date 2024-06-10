use crate::abort_unreachable;
use crate::updaters::Updater;
use dbus::nonblock::{Proxy, SyncConnection};
use futures::{StreamExt, TryStreamExt};
use once_cell::sync::Lazy;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio::{fs, io};
use crate::util::GLOBAL_TOKIO_RUNTIME;

async fn touch(p: impl AsRef<Path>) -> io::Result<()> {
    async fn inner(p: &Path) -> io::Result<()> {
        if let Some(parent) = Path::new(p).parent() {
            fs::create_dir_all(parent).await?
        }
        fs::File::create(p).await.map(drop)
    }

    inner(p.as_ref()).await
}

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

async fn check_network_status() -> Result<bool, dbus::Error> {
    static NETWORK_MANAGER: Lazy<Result<&SyncConnection, dbus::Error>> = Lazy::new(|| {
        let (resource, conn) =
            dbus_tokio::connection::new_session_sync()?;
        
        GLOBAL_TOKIO_RUNTIME.spawn(resource);

        Ok(Arc::leak(conn))
    });

    // Get a proxy to the NetworkManager object
    let proxy = Proxy::new(
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        Duration::from_secs(3),
        NETWORK_MANAGER?,
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
    match check_network_status().await.ok() {
        Some(x) => x,
        None => super::fallback_has_internet().await,
    }
}

async fn ensure_dispatcher_place() -> io::Result<()> {
    let locations = include!("./dispatcher-locations")
        .map(Path::new)
        .map(|loc| loc.join(include_str!("./dispatcher-name")));

    let futures = locations.map(|location| async move {
        if !location.exists() && location.parent().map_or(Ok(false), Path::try_exists)? {
            tokio::fs::write(location, include_bytes!("./dispatcher")).await?;
        }
        Ok(())
    });

    let buffer = futures
        .len()
        .min(thread::available_parallelism().map_or(1, NonZeroUsize::get));
    
    futures::stream::iter(futures)
        .buffer_unordered(buffer)
        .try_collect()
        .await
}

async fn listen(updater: &Updater) -> io::Result<()> {
    touch(include_str!("./socket-path")).await?;
    let listener = UnixListener::bind(include_str!("./socket-path"))?;
    loop {
        let _ = listener.accept().await?;
        if updater.update().is_err() {
            return Ok(());
        }
    }
}

pub fn subscribe(updater: Updater) -> JoinHandle<()> {
    tokio::spawn(async move {
        let res = listen(&updater).await;
        updater.exit(res)
    })
}
