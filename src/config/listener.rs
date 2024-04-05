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
use crate::MessageBoxes;
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
    let (tx, mut rx) = tokio::sync::watch::channel(Ok(vec![]));
    
    const POLL_INTERVAL: Duration = Duration::from_secs(30);

    let _watcher = tokio::task::spawn_blocking(move || {
        macro_rules! exists_or_include {
            ($path: expr, $default: expr) => {
                if !Path::new($path).exists() {
                    std::fs::write($path, include_str!($default))?;
                }
            };
        }
        
        exists_or_include!("./config/sources.toml", "../../default/gen/sources.toml");
        exists_or_include!("./config/config.toml", "../../default/config.toml");
        
        let mut watcher = new_debouncer_opt::<_, RecommendedWatcher, _>(
            POLL_INTERVAL,
            None,
            FsEventHandler(tx),
            FileIdMap::new(),
            notify::Config::default().with_compare_contents(true),
        )?;

        watcher.watcher().watch(Path::new("./config/sources.toml"), RecursiveMode::NonRecursive)?;
        watcher.watcher().watch(Path::new("./config/config.toml"), RecursiveMode::NonRecursive)?;
        Ok::<_, notify::Error>(watcher)
    }).await??;
    
    let shutdown = async {
        let cfg_dropped = async {
            loop {
                if Weak::upgrade(&cfg).is_none() { return }
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
                
                if !events.is_empty() {
                    let res = async {
                        Sources::deserialize_async(
                            &tokio::fs::read_to_string("./config/sources.toml").await?
                        ).await
                    }.await;
                    
                    match res {
                        Ok(ip_sources) => {
                            // TODO: AliMark71
                            let Some(cfg) = Weak::upgrade(&cfg) else { break };
                            let new_cfg = CfgMut { ip_sources };
                            if new_cfg == **cfg.load() { continue }
                            
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

pub async fn subscribe(updaters_manager: &mut UpdatersManager) -> ConfigStorage {
    let msg_bx_handle = updaters_manager.message_boxes().clone();
    let (updater, jh_entry) = updaters_manager.add_updater("config-listener");
    
    let ip_sources = match Sources::from_file("./config/sources.toml").await {
        Ok(x) => x,
        Err(err) => {
            msg_bx_handle.warning(format!("{err}\n\n\n\n...Using default config...")).await;
            Sources::default()
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
    });
    let abort = update_task.abort_handle();
    
    jh_entry.insert(update_task);
    
    ConfigStorage {
        cfg,
        update_task: abort
    }
}