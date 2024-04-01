#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![feature(addr_parse_ascii)]

extern crate core;

use std::borrow::Cow;
use std::net::Ipv4Addr;
use std::panic::AssertUnwindSafe;
use std::pin::pin;
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use anyhow::Context;
use crossbeam::atomic::AtomicCell;
use futures::{StreamExt};
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use tokio::runtime::Runtime;
use tokio::sync::Semaphore;
use crate::prelude::*;
use crate::entity::*;
use crate::retrying_client::RetryingClient;
use err::{err, warn};
use crate::config::Config;
use crate::config::ip_source::GetIpError;
use crate::err::ExitListener;
use crate::network_listener::has_internet;
use crate::updaters::{UpdaterEvent, UpdaterExitStatus, UpdatersManager};
use crate::util::new_skip_interval;

macro_rules! patch_url {
    ($record_id:expr) => {
        format! { concat!(
            "https://api.cloudflare.com/client/v4/zones/",
            include_str!("./secret/zone-id"),
            "/dns_records/{}"
        ), $record_id }
    };
}

macro_rules! static_headers {
    ($(const $name: ident = $val: expr;)*) => {$(
        #[allow(clippy::declare_interior_mutable_const)]
        const $name: HeaderValue = HeaderValue::from_static($val);
    )*};
}

static_headers! {
    const AUTH_EMAIL = include_str!("secret/email"  );
    const AUTH_KEY   = include_str!("secret/api-key");
}

const RECORD: &str = include_str!("secret/record");
const GET_URL: &str = concat!(
    "https://api.cloudflare.com/client/v4/zones/",
    include_str!("secret/zone-id"),
    "/dns_records?type=A&name=",
    include_str!("secret/record")
);


mod prelude;
mod entity;
mod retrying_client;
mod err;
mod network_listener;
mod console_listener;
mod updaters;
mod config;
mod util;

async fn get_ip(client: RetryingClient, cfg: Config) -> Result<Ipv4Addr, GetIpError> {
    let last_err = AtomicCell::new(None);
    
    let iter = cfg.ip_sources().map(|x| x.resolve_ip(&client, &cfg));
    let stream = futures::stream::iter(iter)
        .buffer_unordered(cfg.concurrent_resolve().get() as usize)
        .filter_map(|x| async {
            match x {
                Ok(x) => Some(x),
                Err(err) => {
                    last_err.store(Some(err));
                    None
                }
            }
        });
    
    pin!(stream).next().await.ok_or_else(|| {
        last_err.take().unwrap_or(GetIpError::NoIpSources)
    })
}

struct DdnsContext {
    client: RetryingClient,
    message_boxes: MessageBoxes
}

impl DdnsContext {
    async fn get_record(&self) -> reqwest::Result<OneOrLen<Record>> {
        Ok(
            self.client.get(GET_URL)
                .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
                .header(AUTHORIZATION_KEY, AUTH_KEY)
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .send().await?
                .json::<GetResponse>()
                .await?
                .result
        )
    }

    async fn update_record(&self, id: &str, ip: Ipv4Addr, _cfg: Config) -> anyhow::Result<()> {
        let data = format! {
            r###"{{"type":"A","name":"{record}","content":"{ip}","proxied":false}}"###,
            ip = ip,
            record = include_str!("./secret/record")
        };

        let response = self.client.patch(patch_url!(id))
            .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
            .header(AUTHORIZATION_KEY, AUTH_KEY)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
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
        let ip_task = tokio::spawn(get_ip(self.client.clone(), cfg.clone()));
        let records = self.get_record().await?;
        
        match records {
            OneOrLen::One(Record { id, ip, name}) => {
                anyhow::ensure!(&*name == RECORD, "Expected {RECORD} found {name}");

                let current_ip = ip_task.await??;
                
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
        self.custom_error(move || err(&msg)).await
    }
    
    async fn warning(&self, msg: impl Into<Cow<'static, str>>) {
        let msg = msg.into();
        self.custom_warning(move || warn(&msg)).await
    }
}

async fn real_main() -> ! {
    let ctx = DdnsContext {
        client: RetryingClient::new(),
        message_boxes: MessageBoxes {
            errors_semaphore: Arc::new(Semaphore::new(5)),
            warning_semaphore: Arc::new(Semaphore::new(5))
        }
    };

    let mut updaters_manager = UpdatersManager::new(ctx.message_boxes.clone());
    let mut exit_listener = ExitListener::new();
    
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
                        UpdaterExitStatus::TriggerExit(code) => err::exit(code),
                        UpdaterExitStatus::TriggerRestart => todo!(),
                    }
                }
            },
            _ = exit_listener.recv() => {
                updaters_manager.broadcast_shutdown();
                err::exit(0)
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
        let exit = err::catch_exit(AssertUnwindSafe(|| runtime.block_on(real_main())));
        if let Some(exit) = exit {
            dbg_println!("Shutting down the runtime...");
            drop(runtime);
            dbg_println!("Exiting...");
            return ExitCode::from(exit)
        }
        dbg_println!("Retrying in 15s...");
        thread::sleep(Duration::from_secs(15));
        dbg_println!("Retrying");
    }
}