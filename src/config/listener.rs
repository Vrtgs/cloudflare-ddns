use std::path::Path;
use std::pin::pin;
use std::sync::{Arc, Weak};
use std::time::Duration;
use anyhow::anyhow;
use arc_swap::ArcSwap;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::task::{AbortHandle};
use crate::config::{CfgMut, Config};
use crate::config::ip_source::Sources;
use notify_debouncer_full::{DebounceEventHandler, DebounceEventResult, FileIdMap, new_debouncer_opt};
use crate::{dbg_println, MessageBoxes};
use anyhow::Result;
use crate::updaters::{Updater, UpdatersManager};


pub struct ConfigStorage {
    cfg: Arc<ArcSwap<CfgMut>>,
    update_task: AbortHandle
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


async fn listen(cfg: Weak<ArcSwap<CfgMut>>, updater: &Updater, msg_bx_handle: MessageBoxes) -> Result<()> {
    let (rx, mut tx) = tokio::sync::watch::channel(Ok(vec![]));
    
    const POLL_INTERVAL: Duration = Duration::from_secs(120);

    let _watcher = tokio::task::spawn_blocking(move || {
        if !Path::new("./sources.toml").exists() {
            std::fs::OpenOptions::new()
                .write(true).read(true)
                .create(true).truncate(false)
                .open("./sources.toml")?;
        }
        
        let mut watcher = new_debouncer_opt::<_, RecommendedWatcher, _>(
            POLL_INTERVAL,
            None,
            FsEventHandler(rx),
            FileIdMap::new(),
            notify::Config::default().with_compare_contents(true),
        )?;

        watcher.watcher().watch(Path::new("./sources.toml"), RecursiveMode::NonRecursive)?;
        Ok::<_, notify::Error>(watcher)
    }).await??;
    
    let mut shutdown = pin!(async {
        let cfg_dropped = async {
            loop {
                if Weak::upgrade(&cfg).is_none() { return }
                tokio::time::sleep(Duration::from_secs(10)).await
            }
        };
        
        let shutdown_signal = updater.wait_shutdown();
        
        tokio::select! {
            _ = shutdown_signal => (),
            _ = cfg_dropped => (),
        }
    });

    loop {
        tokio::select! {
            Ok(()) = tx.changed() => {
                let events = {
                    let res = tx.borrow_and_update();
                    res.as_ref().map_err(|e|{
                        e.iter().map(|e| format!("listen event error: {e}")).collect::<Vec<_>>()
                    }).map_err(|e| anyhow!("Error listening to config {e:?}"))?.clone()
                };
                
                dbg_println!("Config Listener: got event {events:?}");
                if !events.is_empty() {
                    let res = async {
                        Sources::deserialize_async(
                            &tokio::fs::read_to_string("./sources.toml").await?
                        ).await
                    }.await;
                    
                    match res {
                        Ok(ip_sources) => {
                            // TODO: AliMark71
                            let Some(cfg) = Weak::upgrade(&cfg) else { break };
                            cfg.store(Arc::new(CfgMut { ip_sources }));
                            if updater.update().is_err() { break }
                        }
                        Err(e) => msg_bx_handle.warning(format!("config listen error: {e}")).await
                    }
                }
            }
            () = &mut shutdown => break,
            else => break
        }
    }
    
    anyhow::Ok(())
}

pub async fn subscribe(updaters_manager: &mut UpdatersManager) -> ConfigStorage {
    let updater = updaters_manager.add_updater("config-listener");
    let msg_bx_handle = updaters_manager.message_boxes().clone();
    
    let ip_sources = match Sources::from_file("./sources.toml").await {
        Ok(x) => x,
        Err(err) => {
            msg_bx_handle.warning(format!("{err}\n\n\n\n...Using default config...")).await;
            Sources::default().await
        }
    };
    
    let cfg = Arc::new(ArcSwap::new(Arc::new(
        CfgMut {
            ip_sources
        }
    )));
    let cfg_weak = Arc::downgrade(&cfg);

    let update_task = tokio::spawn(async move {
        let res = listen(cfg_weak, &updater, msg_bx_handle).await;
        updater.exit(res)
    }).abort_handle();

    ConfigStorage {
        cfg,
        update_task
    }
}