#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::borrow::Cow;
use std::net::{AddrParseError, Ipv4Addr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use ascii::{AsciiStr};
use bytes::Bytes;
use futures::future::select_ok;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use simd_json_derive::Deserialize;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;
use crate::prelude::*;
use crate::entity::*;
use crate::retrying_client::RetryingClient;
use fatal::*;
use crate::updater::UpdaterError;

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


macro_rules! dyn_array {
    (const $name: ident: [$ty:ty] = [$($domain: expr),* $(,)?];) => {
        const $name: [$ty; { ::count_tts::count_tts!($($domain)*) }] = [$($domain),*];
    };
}

dyn_array! {
    const IP_CHECK_DOMAINS: [&str] = [
        "https://checkip.amazonaws.com/",
        "https://api.ipify.org",
        "https://ipv4.icanhazip.com/",
        "https://4.ident.me/",
        "https://v4.tnedi.me/",
        "https://v4.ipv6-test.com/api/myip.php",
        "https://myip.dnsomatic.com/",
        "https://ipinfo.io/ip",
        "https://ipv4.nsupdate.info/myip",
        "https://dynamic.zoneedit.com/checkip.html"
    ];
}

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod prelude;
mod entity;
mod retrying_client;
mod fatal;
mod updater;

#[derive(Debug, Error)]
enum GetIPError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Ipv4Parse(#[from] AddrParseError),
    #[error("invalid data returned")]
    InvalidResponse
}

async fn get_ip(client: &RetryingClient) -> Result<Ipv4Addr, GetIPError> {
    let iter = IP_CHECK_DOMAINS
        .map(|s| client.get(s).header(ACCEPT, HeaderValue::from_static("text/plain")).send())
        .map(|fut| Box::pin(async {
            let ip = fut.await?.bytes().await?;
            if ip.len() > "xxx.xxx.xxx.xxx".len() { return Err(GetIPError::InvalidResponse) }

            let Ok(str) = AsciiStr::from_ascii(&ip)
                else { return Err(GetIPError::InvalidResponse) };

            Ok(Ipv4Addr::from_str(str.as_str())?)
        }));

    select_ok(iter).await.map(|(ip, _)|ip)
}

async fn get_record(client: &RetryingClient) -> Result<OneOrLen<Record>, JsonError> {
    Ok(client.get(GET_URL)
        .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
        .header(AUTHORIZATION_KEY, AUTH_KEY)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .send().await?
        .simd_json::<GetResponse>()
        .await?
        .result)
}

async fn update_record(client: &RetryingClient, id: &str, ip: Ipv4Addr) -> MayPanic<Result<(), Bytes>> {
    let data = format! {
        concat! {
            r###"{{"type":"A","name":""###,
            include_str!("./secret/record"),
            r###"","content":"{ip}","proxied":false}}"###
        }, ip = ip
    };

    let response = client.patch(patch_url!(id))
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

    let response = match PatchResponse::from_slice(&mut bytes.to_vec()) {
        Ok(response) => response,
        Err(err) => return Ok(Err(Bytes::from(format!("unable to deserialize json due to: {err:?}"))))
    };

    match failure || !response.success {
        false => Ok(Ok(())),
        true  => Ok(Err(bytes)),
    }
}

async fn run_ddns(client: &RetryingClient) -> MayPanic<()> {
    let ip_task = tokio::spawn({
        let client = client.clone();
        async move { get_ip(&client).await }
    });

    let records = get_record(client).await
        .map_err(|err| err!(f!"{err:?}", -222))?;


    match records {
        OneOrLen::One(Record { id, ip, name}) => {
            assert!(&*name == RECORD, f!"Expected {RECORD} found {name}", 99)?;

            let current_ip = ip_task.await
                .map_err(|err| err!(f!"Join Error: {err}", -333))?
                .map_err(|err| err!(f!"Get Ip Error: {err}", -444))?;

            let parsed_ip = Ipv4Addr::from_str(&ip);
            match parsed_ip {
                Err(_) => warn(&format!("cloudflare returned invalid ip: {ip}")),
                Ok(ip) if ip == current_ip => return Ok(()),
                Ok(_)  => {}
            }

            let response = update_record(client, &id, current_ip).await?;

            if let Err(err) = response {
                let string = String::from_utf8_lossy(err.as_ref());

                let path = std::env::temp_dir()
                    .join(Uuid::new_v4().to_string())
                    .with_extension("log");

                tokio::fs::write(&path, &*string).await
                    .map_err(|err| err!(f!"FATAL WRITE ERR: {err:?}", -666))?;

                Command::new("notepad").arg(&path).spawn()
                    .map_err(|err| err!(f!"FATAL SPAWN ERR: {err:?}", -777))?
                    .wait().await
                    .map_err(|err| err!(f!"FATAL WAIT ERR: {err:?}", -777))?;

                let _ = tokio::fs::remove_file(&path).await;
            }

            Ok(())
        },
        OneOrLen::Len(len) => Err(err!(f!"Expected 1 record Got {len}", 99))
    }
}

fn real_main() -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build().expect("Failed to set up async context");

    let client = RetryingClient::new();

    runtime.block_on(async {
        let mut interval = tokio::time::interval(Duration::from_secs(60 * 60)); // 1 hour
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        
        let (shutdown_sender, mut shutdown_receiver) = 
            tokio::sync::oneshot::channel::<UpdaterError>();
        let notify = Arc::new(Notify::new());
        
        updater::subscribe(Arc::clone(&notify), shutdown_sender);

        loop {
            let run = || async {
                dbg_println!("updating");

                match run_ddns(&client).await {
                    Err(panic) => {
                        dbg_println!("responsibly panicking");
                        let _ = std::panic::catch_unwind(panic);
                        dbg_println!("caught panic");
                    },

                    Ok(()) => dbg_println!("successfully updated")
                }
            };
            tokio::select! {
                _ = notify.notified() => interval.reset_immediately(),
                _ = interval.tick() => run().await,
                res = &mut shutdown_receiver => {
                    let msg = match res {
                        Ok(err) => format!("{err}").into(),
                        Err(_) => Cow::from("update task died unexpectedly")
                    };
                    
                    runtime.spawn_blocking(move || err(&msg));
                    loop {
                        interval.tick().await;
                        run().await
                    }
                }
            }
        }
    })
}

fn main() -> ! {
    set_hook();
    real_main();
}