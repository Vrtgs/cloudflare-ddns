use crate::config::ip_source::Sources;
use crate::config::{ApiFields, CfgInner, Config};
use crate::updaters::{Updater, UpdatersManager};
use crate::{UserMessages, util};
use anyhow::Result;
use anyhow::{anyhow, Context};
use arc_swap::ArcSwap;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{
    new_debouncer_opt, DebounceEventHandler, DebounceEventResult, FileIdMap,
};
use std::io;
use std::path::Path;
use std::pin::pin;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::task::AbortHandle;

pub struct ConfigStorage {
    cfg: Arc<ArcSwap<CfgInner>>,
    update_task: AbortHandle,
}

impl Drop for ConfigStorage {
    fn drop(&mut self) {
        self.update_task.abort()
    }
}

impl ConfigStorage {
    #[inline]
    pub fn load_config(&self) -> Config {
        Config(self.cfg.load_full())
    }
}

struct FsEventHandler(tokio::sync::watch::Sender<DebounceEventResult>);

impl DebounceEventHandler for FsEventHandler {
    fn handle_event(&mut self, event: DebounceEventResult) {
        let _ = self.0.send(event);
    }
}

async fn listen(
    cfg: Weak<ArcSwap<CfgInner>>,
    updater: &Updater,
    msg_bx_handle: UserMessages,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::watch::channel(Ok(vec![]));

    const POLL_INTERVAL: Duration = Duration::from_secs(30);

    let _watcher = tokio::task::spawn_blocking(move || {
        let mut watcher = new_debouncer_opt::<_, RecommendedWatcher, _>(
            POLL_INTERVAL,
            None,
            FsEventHandler(tx),
            FileIdMap::new(),
            notify::Config::default().with_compare_contents(true),
        )?;

        watcher.watcher().watch(
            Path::new("./config/sources.toml"),
            RecursiveMode::NonRecursive,
        )?;
        watcher.watcher().watch(
            Path::new("./config/config.toml"),
            RecursiveMode::NonRecursive,
        )?;
        anyhow::Ok(watcher)
    })
    .await??;

    let shutdown = async {
        let cfg_dropped = async {
            loop {
                if Weak::upgrade(&cfg).is_none() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(10)).await
            }
        };

        tokio::select! {
            _ = updater.wait_shutdown() => (),
            _ = cfg_dropped => (),
        }
    };
    let mut shutdown = pin!(shutdown);

    loop {
        tokio::select! {
            Ok(()) = rx.changed() => {
                let events = {
                    let borrow = rx.borrow_and_update();
                    borrow.as_ref().map_err(|e|{
                        e.iter().map(|e| format!("listen event error: {e}")).collect::<Vec<_>>()
                    }).map_err(|e| anyhow!("Error listening to config {e:?}")).cloned()
                };

                let events = match events {
                    Ok(events) => events,
                    Err(e) => {
                        msg_bx_handle.error(e.to_string()).await;
                        continue
                    },
                };

                macro_rules! change_on_occurred {
                    ($path:literal in $events:expr) => { $events.iter().any(|e| e.paths.iter().any(|p| p.ends_with($path))) };
                }

                if change_on_occurred!("sources.toml" in events) {
                    let res = async {
                        Sources::deserialize_async(
                            &tokio::fs::read_to_string("./config/sources.toml").await?
                        ).await
                    }.await;

                    match res {
                        Ok(ip_sources) => {
                            let Some(cfg) = Weak::upgrade(&cfg) else { break };
                            let old_cfg = cfg.load();
                            if ip_sources == *old_cfg.ip_sources { continue }

                            let new_cfg = CfgInner::new(Arc::clone(&old_cfg.api_fields), ip_sources);
                            cfg.store(Arc::new(new_cfg));
                            if updater.update().is_err() { break }
                        }
                        Err(e) => msg_bx_handle.warning(format!("config listen error: {e}")).await
                    }
                }

                if change_on_occurred!("config.toml" in events) {
                    let res = ApiFields::deserialize(&tokio::fs::read_to_string("./config/config.toml").await?);

                    match res {
                        Ok(api_fields) => {
                            let Some(cfg) = Weak::upgrade(&cfg) else { break };
                            let old_cfg = cfg.load();
                            if api_fields == *old_cfg.api_fields { continue }


                            let new_cfg = CfgInner::new(api_fields, Arc::clone(&old_cfg.ip_sources));
                            cfg.store(Arc::new(new_cfg));
                            if updater.update().is_err() { break }
                        }
                        Err(e) => msg_bx_handle.warning(format!("config listen error: {e}")).await
                    }
                }
            }
            _ = &mut shutdown => break,
            else => break
        }
    }

    anyhow::Ok(())
}

pub async fn subscribe(updaters_manager: &mut UpdatersManager) -> Result<ConfigStorage> {
    let user_messages = updaters_manager.user_messages().clone();
    let (updater, jh_entry) = updaters_manager.add_updater("config-listener");

    if !util::try_exists("./config").await? {
        tokio::fs::create_dir_all("./config").await?;
    }
    if !tokio::fs::metadata("./config").await?.is_dir() {
        anyhow::bail!("./config is not a directory")
    }

    macro_rules! exists_or_include {
        ($($path: expr, $default: expr $(;)+)*) => {
            tokio::try_join!($(async {
                if !util::try_exists($path).await? {
                    tokio::fs::write($path, include_str!($default)).await?;
                }
                Ok::<_, io::Error>(())
            }),*)
        };
    }

    exists_or_include!(
        "./config/sources.toml", "../../default/gen/sources.toml";
        "./config/config.toml", "../../default/config.toml";
    )?;

    let ip_sources = match Sources::from_file("./config/sources.toml").await {
        Ok(x) => x,
        Err(err) => {
            user_messages
                .warning(format!("{err}\n\n\n\n...Using default config..."))
                .await;
            Sources::default()
        }
    };

    let api_fields = ApiFields::from_file("./config/config.toml")
        .await
        .with_context(|| "Invalid API Fields Config")?;

    let cfg = Arc::new(ArcSwap::new(Arc::new(CfgInner::new(
        api_fields, ip_sources,
    ))));
    let cfg_weak = Arc::downgrade(&cfg);

    let update_task = tokio::spawn(async move {
        let res = listen(cfg_weak, &updater, user_messages).await;
        updater.exit(res)
    });
    let abort = update_task.abort_handle();

    jh_entry.insert(update_task);

    Ok(ConfigStorage {
        cfg,
        update_task: abort,
    })
}
