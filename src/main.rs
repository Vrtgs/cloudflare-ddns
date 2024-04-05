#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![feature(addr_parse_ascii)]

extern crate core;

use std::borrow::Cow;
use std::cell::Cell;
use std::net::Ipv4Addr;
use std::pin::pin;
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use anyhow::Context;
use futures::{StreamExt};
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use tokio::runtime::Runtime;
use tokio::sync::Semaphore;
use tokio::try_join;
use crate::prelude::*;
use crate::entity::*;
use crate::retrying_client::RetryingClient;
use crate::config::Config;
use crate::config::ip_source::GetIpError;
use crate::network_listener::has_internet;
use crate::updaters::{UpdaterEvent, UpdaterExitStatus, UpdatersManager};
use crate::util::new_skip_interval;


macro_rules! from_static {
    ($(const $name: ident: $ty: ty = $val: expr;)*) => {$(
        #[allow(clippy::declare_interior_mutable_const)]
        const $name: $ty = <$ty>::from_static($val);
    )*};
}

from_static! {
    const AUTH_EMAIL: HeaderValue = include_str!("secret/email");
    const AUTH_KEY  : HeaderValue = include_str!("secret/api-key");

    const JSON_MIME: HeaderValue  = "application/json";
}

mod prelude;
mod entity;
mod retrying_client;
mod err;
mod network_listener;
mod console_listener;
mod updaters;
mod config;
mod util;

struct DdnsContext {
    client: RetryingClient,
    message_boxes: MessageBoxes
}

impl DdnsContext {
    async fn get_ip(&self, cfg: Config) -> anyhow::Result<Ipv4Addr> {
        let last_err = Cell::new(None);

        let iter = cfg.ip_sources().map(|x| x.resolve_ip(&self.client, &cfg));
        let stream = futures::stream::iter(iter)
            .buffer_unordered(cfg.concurrent_resolve().get() as usize)
            .filter_map(|x| std::future::ready({
                match x {
                    Ok(x) => Some(x),
                    Err(err) => {
                        last_err.set(Some(err));
                        None
                    }
                }
            }));

        pin!(stream).next().await.ok_or_else(|| {
            last_err.take().unwrap_or(GetIpError::NoIpSources).into()
        })
    }

    async fn get_record(&self, _cfg: Config) -> anyhow::Result<OneOrLen<Record>> {
        let url = format!(
            "https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records?type=A&name={record}",
            zone_id= include_str!("./secret/zone-id"),
            record = include_str!("./secret/record")
        );
        
        Ok(
            self.client.get(url)
                .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
                .header(AUTHORIZATION_KEY, AUTH_KEY)
                .header(CONTENT_TYPE, JSON_MIME)
                .send().await?
                .json::<GetResponse>()
                .await?
                .result
        )
    }

    async fn update_record(&self, id: &str, ip: Ipv4Addr, _cfg: Config) -> anyhow::Result<()> {
        let data = format! {
            r###"{{"type":"A","name":"{record}","content":"{ip}","proxied":{proxied}}}"###,
            ip = ip,
            record = include_str!("./secret/record"),
            proxied = false
        };

        let url = format! {
            "https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records/{record_id}",
            zone_id = include_str!("./secret/zone-id"),
            record_id = id
        };

        let response = self.client.patch(url)
            .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
            .header(AUTHORIZATION_KEY, AUTH_KEY)
            .header(CONTENT_TYPE, JSON_MIME)
            .body(data)
            .send().await?;

        let failure = !response.status().is_success();

        let bytes = response.bytes().await.with_context(|| "unable to retrieve bytes")?;
        
        let response = serde_json::from_slice::<PatchResponse>(&bytes)
            .with_context(|| "unable to deserialize patch response json")?;

        if failure || !response.success {
            anyhow::bail!("Bad response: {}", String::from_utf8_lossy(&bytes))
        }
        Ok(())
    }

    pub async fn run_ddns(&self, cfg: Config) -> anyhow::Result<()> {
        let (current_ip, record) = try_join!(
            self.get_ip(cfg.clone()), self.get_record(cfg.clone())
        )?;
        
        match record {
            OneOrLen::One(Record { id, ip, name}) => {
                anyhow::ensure!(&*name == include_str!("secret/record"), "Expected {} found {name}", include_str!("secret/record"));

                match Ipv4Addr::from_str(&ip) {
                    Err(_) => self.message_boxes.warning(format!("cloudflare returned an invalid ip: {ip}")).await,
                    Ok(ip) if ip == current_ip => {
                        dbg_println!("IP didn't change skipping record update");
                        return Ok(());
                    },
                    Ok(_)  => {}
                }

                self.update_record(&id, current_ip, cfg).await
            },
            OneOrLen::Len(len) => anyhow::bail!("Expected 1 record Got {len}")
        }
    }
}

#[derive(Clone)]
struct MessageBoxes {
    errors_semaphore: Arc<Semaphore>,
    warning_semaphore: Arc<Semaphore>
}

impl MessageBoxes {
    async fn custom_error(&self, fun: impl FnOnce() + Send + 'static) {
        err::spawn_message_box(Arc::clone(&self.errors_semaphore), fun).await
    }

    async fn custom_warning(&self, fun: impl FnOnce() + Send + 'static) {
        err::spawn_message_box(Arc::clone(&self.warning_semaphore), fun).await
    }

    async fn error(&self, msg: impl Into<Cow<'static, str>>) {
        let msg = msg.into();
        self.custom_error(move || err::err(&msg)).await
    }
    
    async fn warning(&self, msg: impl Into<Cow<'static, str>>) {
        let msg = msg.into();
        self.custom_warning(move || err::warn(&msg)).await
    }
}

enum Action {
    Restart,
    Exit(u8)
}

async fn real_main() -> Action {
    let ctx = DdnsContext {
        client: RetryingClient::new(),
        message_boxes: MessageBoxes {
            errors_semaphore: Arc::new(Semaphore::new(5)),
            warning_semaphore: Arc::new(Semaphore::new(5))
        }
    };

    let mut updaters_manager = UpdatersManager::new(ctx.message_boxes.clone());

    err::exit::subscribe(&mut updaters_manager);
    network_listener::subscribe(&mut updaters_manager);
    console_listener::subscribe(&mut updaters_manager);
    let cfg_store = config::listener::subscribe(&mut updaters_manager).await;


    // 1 hour
    // this will be controlled by config
    let mut interval = new_skip_interval(Duration::from_secs(60 * 60));
    
    
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if !has_internet().await {
                    dbg_println!("no internet available skipping update");
                    continue;
                }
                
                dbg_println!("updating");
                match ctx.run_ddns(cfg_store.load_config()).await {
                    Err(panic) => ctx.message_boxes.error(panic.to_string()).await,
                    Ok(()) => dbg_println!("successfully updated")
                }
            },
            res = updaters_manager.watch() => match res {
                UpdaterEvent::Update => interval.reset_immediately(),
                UpdaterEvent::ServiceEvent(exit) => {
                    match exit.status {
                        UpdaterExitStatus::Success => {},
                        UpdaterExitStatus::Panic | UpdaterExitStatus::Error(_) => {
                            ctx.message_boxes.error(format!("Updater abruptly exited: {exit}")).await
                        }
                        UpdaterExitStatus::TriggerExit(code) => {
                            updaters_manager.shutdown().await;
                            return Action::Exit(code);
                        },
                        UpdaterExitStatus::TriggerRestart => return Action::Restart,
                    }
                }
            }
        }
    }

}

#[cfg(feature = "trace")] 
fn make_runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all().build().expect("failed ot build runtime")
}

#[cfg(not(feature = "trace"))]
fn make_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all().build().expect("failed ot build runtime")
}

fn main() -> ExitCode {
    err::set_hook();
    #[cfg(feature = "trace")] console_subscriber::init();

    let runtime = make_runtime();
    loop {
        let exit = std::panic::catch_unwind(|| runtime.block_on(real_main()));
        
        match exit {
            Ok(Action::Exit(exit)) => {
                dbg_println!("Shutting down the runtime...");
                drop(runtime);
                dbg_println!("Exiting...");
                return ExitCode::from(exit)
            }
            Ok(Action::Restart) => dbg_println!("Restarting..."),
            Err(_) => {
                dbg_println!("Panicked!!");
                dbg_println!("Retrying in 15s...");
                thread::sleep(Duration::from_secs(15));
                dbg_println!("Retrying")
            }
        }
    }
}