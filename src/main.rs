#![feature(addr_parse_ascii)]
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use bytes::Bytes;
use futures::future::select_ok;
use futures::FutureExt;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use tokio::sync::Semaphore;
use tokio::time::{Instant, Interval, MissedTickBehavior};
use crate::prelude::*;
use crate::entity::*;
use crate::retrying_client::RetryingClient;
use fatal::*;
use crate::config::{CONFIG, GetIpError};
use crate::network_listener::has_internet;
use crate::updaters::{UpdaterEvent, UpdatersManager};

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
mod fatal;
mod network_listener;
mod updaters;
mod config;
mod util;


struct DdnsContext {
    client: RetryingClient,
    errors_semaphore: Arc<Semaphore>,
    warning_semaphore: Arc<Semaphore>
}

impl DdnsContext {
    async fn get_ip(client: RetryingClient) -> Result<Ipv4Addr, GetIpError> {
        let cfg = CONFIG.read().await;
        let iter = cfg.ip_sources.iter()
            .map(|x| Box::pin(x.resolve_ip(&client)));

        select_ok(iter).await.map(|(ip, _)|ip)
    }

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

    async fn update_record(&self, id: &str, ip: Ipv4Addr) -> MayPanic<Result<(), Bytes>> {
        let data = format! {
            concat! {
                r###"{{"type":"A","name":""###,
                include_str!("./secret/record"),
                r###"","content":"{ip}","proxied":false}}"###
            }, ip = ip
        };

        let response = self.client.patch(patch_url!(id))
            .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
            .header(AUTHORIZATION_KEY, AUTH_KEY)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(data)
            .send().await
            .map_err(|err| err!(f!"{err:?}", -444))?;

        let failure = !response.status().is_success();

        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => return Ok(Err(Bytes::from(format!("unable to retrieve bytes because: {err:?}"))))
        };

        let response = match serde_json::from_slice::<PatchResponse>(&bytes) {
            Ok(response) => response,
            Err(err) => return Ok(Err(Bytes::from(format!("unable to deserialize json due to: {err:?}"))))
        };

        match failure || !response.success {
            false => Ok(Ok(())),
            true  => Ok(Err(bytes)),
        }
    }

    pub async fn run_ddns(&self) -> MayPanic<()> {
        let ip_task = tokio::spawn(Self::get_ip(self.client.clone()));
        
        let records = self.get_record().await
            .map_err(|err| err!(f!"{err:?}", -222))?;
        
        match records {
            OneOrLen::One(Record { id, ip, name}) => {
                assert!(&*name == RECORD, f!"Expected {RECORD} found {name}", 99)?;

                let current_ip = ip_task.await
                    .map_err(|err| err!(f!"Join Error: {err}", -333))?
                    .map_err(|err| err!(f!"Get Ip Error: {err}", -444))?;

                match Ipv4Addr::from_str(&ip) {
                    Err(_) => {
                        tokio::spawn(spawn_message_box(
                            Arc::clone(&self.warning_semaphore),
                            move || warn(&format!("cloudflare returned invalid ip: {ip}")),
                        ));
                    },
                    Ok(ip) if ip == current_ip => return Ok(()),
                    Ok(_)  => {}
                }

                self.update_record(&id, current_ip).await?.map_err(|e| { 
                    err!(f!("Could not update record got response: {}", String::from_utf8_lossy(&e)), -1055)
                })
            },
            OneOrLen::Len(len) => Err(err!(f!"Expected 1 record Got {len}", 99))
        }
    }
    
}

#[inline]
fn new_interval(period: Duration) -> Interval {
    new_interval_at(Instant::now(), period)
}

fn new_interval_at(start: Instant, period: Duration) -> Interval {
    let mut interval = tokio::time::interval_at(start, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}

#[tokio::main]
async fn real_main() -> ! {
    let ctx = DdnsContext {
        client: RetryingClient::new(),
        errors_semaphore: Arc::new(Semaphore::new(5)),
        warning_semaphore: Arc::new(Semaphore::new(5)),
    };
    
    let mut interval = new_interval(Duration::from_secs(60 * 60)); // 1 hour
    let mut updaters_manager = UpdatersManager::new();
    
    macro_rules! show_err {
        ($fn: expr) => { spawn_message_box(Arc::clone(&ctx.errors_semaphore), $fn).await };
    }

    network_listener::subscribe(&mut updaters_manager);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if !has_internet() {
                    interval.reset_after((interval.period()/8).max(Duration::from_secs(5)));
                    dbg_println!("no internet available skipping update"); continue;
                }
            
                dbg_println!("updating");
                match ctx.run_ddns().await {
                    Err(panic) => show_err!(move || {
                        dbg_println!("responsibly panicking");
                        match std::panic::catch_unwind(panic) {
                            Ok(never) => match never {  },
                            Err(_) => dbg_println!("caught panic")
                        }
                    }),
                    Ok(()) => dbg_println!("successfully updated")
                }
            },
            res = updaters_manager.watch() => match res {
                UpdaterEvent::Update => interval.reset_immediately(),
                UpdaterEvent::ServiceExited(status) =>
                    show_err!(move || err(&format!("{status}")))
            },
        }
    }
}

fn main() -> ! {
    set_hook();
    loop {
        let _ = match std::panic::catch_unwind(real_main) {
            Ok(never) => never,
            
            // should be handled by the panic hook
            Err(e) => e
        };
    }
}