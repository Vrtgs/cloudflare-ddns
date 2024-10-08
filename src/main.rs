#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

extern crate core;

use crate::config::ip_source::GetIpError;
use crate::config::Config;
use crate::network_listener::has_internet;
use crate::retrying_client::RetryingClient;
use crate::updaters::{UpdaterEvent, UpdaterExitStatus};
use crate::util::{new_skip_interval, EscapeExt};
use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use serde::Deserialize;
use std::borrow::Cow;
use std::cell::Cell;
use std::net::Ipv4Addr;
use std::num::NonZeroU8;
use std::panic::AssertUnwindSafe;
use std::pin::pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::thread::Builder;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::try_join;

mod config;
mod console_listener;
mod err;
mod network_listener;
mod pre;
mod retrying_client;
mod updaters;
mod util;

struct DdnsContext {
    client: RetryingClient,
    user_messages: UserMessages,
}

#[derive(Debug)]
struct Record {
    id: Box<str>,
    ip: Ipv4Addr,
}

impl DdnsContext {
    fn new(cfg: Config) -> Self {
        DdnsContext {
            client: RetryingClient::new(&cfg),
            user_messages: UserMessages::new(cfg.misc().general().max_errors()),
        }
    }

    async fn get_ip(&self, cfg: &Config) -> Result<Ipv4Addr> {
        let last_err = Cell::new(None);

        let iter = cfg.ip_sources().map(|x| x.resolve_ip(&self.client, cfg));
        let stream = futures::stream::iter(iter)
            .buffer_unordered(cfg.concurrent_resolve().get() as usize)
            .filter_map(|x| {
                std::future::ready({
                    match x {
                        Ok(x) => Some(x),
                        Err(err) => {
                            last_err.set(Some(err));
                            None
                        }
                    }
                })
            });

        pin!(stream)
            .next()
            .await
            .ok_or_else(|| last_err.take().unwrap_or(GetIpError::NoIpSources).into())
    }

    async fn get_record(&self, cfg: &Config) -> Result<Record> {
        let url = format!(
            "https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records?type=A&name={record}",
            zone_id = cfg.zone().id(),
            record = cfg.zone().record()
        );

        #[derive(Debug, Deserialize)]
        struct FullATypeRecord {
            id: Box<str>,
            name: Box<str>,
            #[serde(rename = "content")]
            ip: Ipv4Addr,
        }

        #[derive(Debug, Deserialize)]
        pub struct GetResponse {
            result: Vec<FullATypeRecord>,
        }

        let records = cfg
            .authorize_request(self.client.get(url))
            .send()
            .await?
            .json::<GetResponse>()
            .await?
            .result;

        let [FullATypeRecord { id, ip, name }] = <[FullATypeRecord; 1]>::try_from(records)
            .map_err(|vec| anyhow!("expected 1 record got {} records: {vec:?}", vec.len()))?;

        anyhow::ensure!(
            &*name == cfg.zone().record(),
            "Expected {} found {name}",
            cfg.zone().record()
        );

        Ok(Record { id, ip })
    }

    async fn update_record(&self, id: &str, ip: Ipv4Addr, cfg: &Config) -> Result<()> {
        let request_json = format! {
            r###"{{"type":"A","name":"{record}","content":"{ip}","proxied":{proxied}}}"###,
            record = cfg.zone().record().escape_json(),
            proxied = cfg.zone().proxied()
        };

        let url = format! {
            "https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records/{record_id}",
            zone_id = cfg.zone().id(),
            record_id = id
        };

        let response = cfg
            .authorize_request(self.client.patch(url))
            .json(request_json)
            .send()
            .await?;

        let failure = !response.status().is_success();

        let bytes = response
            .bytes()
            .await
            .with_context(|| "unable to retrieve bytes")?;

        #[derive(Debug, Deserialize)]
        pub struct PatchResponse {
            success: bool,
        }

        let response = serde_json::from_slice::<PatchResponse>(&bytes)
            .with_context(|| "unable to deserialize patch response json")?;

        if failure || !response.success {
            anyhow::bail!("Bad response: {}", String::from_utf8_lossy(&bytes))
        }

        Ok(())
    }

    pub async fn run_ddns(&self, cfg: Config) -> Result<bool> {
        let (record, current_ip) = try_join!(self.get_record(&cfg), self.get_ip(&cfg))?;

        if record.ip == current_ip {
            return Ok(false);
        }

        self.update_record(&record.id, current_ip, &cfg).await?;
        Ok(true)
    }
}

#[derive(Clone)]
struct UserMessages {
    errors: Arc<Semaphore>,
    warning: Arc<Semaphore>,
}

impl UserMessages {
    fn new(max_errors: NonZeroU8) -> Self {
        let permits = max_errors.get() as usize;
        UserMessages {
            errors: Arc::new(Semaphore::new(permits)),
            warning: Arc::new(Semaphore::new(permits)),
        }
    }

    async fn custom_error(&self, fun: impl FnOnce() + Send + 'static) {
        err::spawn_message_box(Arc::clone(&self.errors), fun).await
    }

    async fn custom_warning(&self, fun: impl FnOnce() + Send + 'static) {
        err::spawn_message_box(Arc::clone(&self.warning), fun).await
    }

    async fn error(&self, msg: impl Into<Cow<'static, str>>) {
        let msg = msg.into();
        self.custom_error(move || err::error(&msg)).await
    }

    async fn warning(&self, msg: impl Into<Cow<'static, str>>) {
        let msg = msg.into();
        self.custom_warning(move || err::warn(&msg)).await
    }
}

enum Action {
    Restart,
    Exit(u8),
}

async fn real_main() -> Result<Action> {
    let (ctx, mut updaters_manager, cfg_store) = config::listener::load().await?;
    let network_detection = cfg_store.load_config().misc().refresh().network_detection();

    if network_detection {
        network_listener::subscribe(&mut updaters_manager)?;
    }
    err::exit::subscribe(&mut updaters_manager)?;
    console_listener::subscribe(&mut updaters_manager)?;

    let mut interval = new_skip_interval(cfg_store.load_config().misc().refresh().interval());

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if !has_internet().await {
                    dbg_println!("no internet available skipping update");
                    continue;
                }

                dbg_println!("updating");
                match ctx.run_ddns(cfg_store.load_config()).await {
                    Err(err) => ctx.user_messages.error(err.to_string()).await,
                    Ok(true) => dbg_println!("successfully updated"),
                    Ok(false) => dbg_println!("IP didn't change skipping record update"),
                }
            },
            res = updaters_manager.watch() => match res {
                UpdaterEvent::Update => interval.reset_immediately(),
                UpdaterEvent::ServiceEvent(exit) => {
                    match *exit.status() {
                        UpdaterExitStatus::Success => {},
                        UpdaterExitStatus::Panic | UpdaterExitStatus::Error(_) => {
                            ctx.user_messages.error(format!("Updater abruptly exited: {exit}")).await
                        }
                        UpdaterExitStatus::TriggerExit(code) => {
                            updaters_manager.shutdown().await;
                            return Ok(Action::Exit(code));
                        },
                        UpdaterExitStatus::TriggerRestart => return Ok(Action::Restart),
                    }
                }
            }
        }
    }
}

#[cfg(feature = "trace")]
fn make_runtime() -> tokio::runtime::Handle {
    (*util::GLOBAL_TOKIO_RUNTIME).clone()
}

#[cfg(not(feature = "trace"))]
fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(util::num_cpus().get())
        .enable_all()
        .build()
        .expect("failed to build runtime")
}

fn main() -> ExitCode {
    pre::pre_run();
    #[cfg(feature = "trace")]
    console_subscriber::init();

    let mut runtime = make_runtime();
    loop {
        let exit = std::panic::catch_unwind(AssertUnwindSafe(|| runtime.block_on(real_main())));

        match exit {
            // Non-Recoverable
            Ok(Ok(Action::Exit(exit))) => {
                dbg_println!("Shutting down the runtime...");
                drop(runtime);
                dbg_println!("Exiting...");
                return ExitCode::from(exit);
            }
            Ok(Err(e)) => {
                dbg_println!("Fatal init error");
                dbg_println!("Aborting...");
                // best effort clean up
                let _ = Builder::new().spawn(move || drop(runtime));
                abort!("{e}")
            }

            // Recoverable
            Ok(Ok(Action::Restart)) => dbg_println!("Restarting..."),
            Err(_) => {
                // old runtime might be in an invalid state
                // replace it and drop it on a new thread to avoid hanging
                let old_runtime = std::mem::replace(&mut runtime, make_runtime());
                thread::spawn(move || drop(old_runtime));

                dbg_println!("Panicked!!");
                dbg_println!("Retrying in 15s...");
                thread::sleep(Duration::from_secs(15));
                dbg_println!("Retrying")
            }
        }
    }
}
