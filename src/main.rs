#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

use std::net::{AddrParseError, Ipv4Addr};
use std::process::exit;
use std::str::FromStr;
use std::time::Duration;
use bytes::Bytes;
use futures::future::select_ok;
use futures::FutureExt;
use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use tokio::process::Command;
use uuid::Uuid;
use windows::core::{PCWSTR, w};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use crate::prelude::*;
use crate::entity::*;

macro_rules! retry {
    ($fut:expr) => {async {
        match $fut.await {
            Ok(res) => Ok(res),
            Err(..) => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                match $fut.await {
                    Ok(res) => Ok(res),
                    err @ Err(_) => err,
                }
            }
        }
    }};
}

macro_rules! assert {
    ($cond:expr, $($tokens:tt)*) => {
        if !$cond {
            panic!($($tokens)*)
        }
    };
}

macro_rules! panic {
    ($err_msg:literal, $code:literal) => {
        exit_err_inner(w!($err_msg), $code)
    };
    (f!$err_msg:tt, $code:literal) => {
        exit_err(&format!($err_msg), $code)
    };
}

macro_rules! patch_json {
    ($val:expr) => {
        format! {
            concat!{
                r###"{{"type":"A","name":""###,
                include_str!("./secret/record"),
                r###"","content":"{}","proxied":false}}"###
            }, $val
        }
    };
}

macro_rules! patch_url {
    ($record_id:expr) => {
        format! {concat!(
            "https://api.cloudflare.com/client/v4/zones/",
            include_str!("./secret/zone-id"),
            "/dns_records/{}"
        ), $record_id}
    };
}

#[allow(clippy::declare_interior_mutable_const)]
const AUTH_EMAIL: HeaderValue = HeaderValue::from_static("mirza123.the.best@gmail.com");
#[allow(clippy::declare_interior_mutable_const)]
const AUTH_KEY: HeaderValue = HeaderValue::from_static(include_str!("./secret/api-key"));


const RECORD: &str = include_str!("./secret/record");
const GET_URL: &str = concat!(
    "https://api.cloudflare.com/client/v4/zones/",
    include_str!("./secret/zone-id"),
    "/dns_records?type=A&name=",
    include_str!("./secret/record")
);

const IP_CHECK_DOMAINS: [&str; 3] = [
    "https://checkip.amazonaws.com/",
    "https://api.ipify.org",
    "https://ipv4.icanhazip.com/"
];

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod prelude;
mod entity;

#[derive(Debug)]
enum GetIPError {
    Reqwest(reqwest::Error),
    Ipv4Parse(AddrParseError)
}
async fn get_ip(client: &Client) -> Result<String, GetIPError> {
    let iter = IP_CHECK_DOMAINS
        .map(|s| client.get(s).send())
        .map(|fut| async {
            fut.await.map_err(GetIPError::Reqwest)?.text().await
                .map_err(GetIPError::Reqwest)
                .and_then(|s| {
                    if let Err(err) = Ipv4Addr::from_str(&s) {
                        Err(GetIPError::Ipv4Parse(err))
                    } else { Ok(s) }
                })
        }.boxed());

    select_ok(iter).await.map(|(ip, _)|ip)
}

#[cfg(windows)]
#[inline]
fn exit_err_inner(err_msg: PCWSTR, code: i32) -> ! {
    unsafe {
        MessageBoxW(
            None,
            err_msg,
            w!("CloudFlare DDNS Error"),
            MB_OK | MB_ICONERROR
        );
    }

    exit(code);
}

#[cfg(windows)]
fn exit_err(err: &str, code: i32) -> ! {
    let err_msg = err.encode_utf16()
        .chain(std::iter::once(0)).collect::<Vec<_>>();

    exit_err_inner(PCWSTR::from_raw(err_msg.as_ptr()), code)
}

async fn get_records(client: &Client) -> Result<Vec<Record>, JsonError> {
    Ok(client.get(GET_URL)
        .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
        .header(AUTHORIZATION_KEY, AUTH_KEY)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .send().await.map_err(JsonError::Reqwest)?
        .json::<GetResponse>()
        .await?
        .result)
}

async fn update_record(client: &Client, id: &str, ip: &str) -> Result<(), Bytes> {
    let data = patch_json!(ip);

    let response = client.patch(patch_url!(id))
        .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
        .header(AUTHORIZATION_KEY, AUTH_KEY)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .body(data)
        .send().await
        .unwrap_or_else(|err| panic!(f!"{err:?}", -444));

    let mut failed = !response.status().is_success();

    let bytes = match response.bytes().await {
        Ok(bytes) => {bytes}
        Err(err) => return Err(Bytes::from(format!("unable to retrieve bytes because: {err:?}")))
    };

    let response = match simd_json::from_reader::<_, PatchResponse>(bytes.as_ref()) {
        Ok(response) => response,
        Err(err) => return Err(Bytes::from(format!("unable to deserialize json due to: {err:?}")))
    };

    failed |= !response.success;

    match failed {
        false => Ok(()),
        true  => Err(bytes),
    }
}

#[tokio::main]
async fn real_main() {
    let client = Client::new();

    let ip_task = tokio::spawn({
        let client = client.clone();
        async move {
            match retry!((retry!(get_ip(&client)))).await {
                Ok(ip) => ip,
                Err(err) => { panic!(f!"{err:?}", -111) }
            }
        }
    });


    let records = retry!(retry!(get_records(&client))).await
        .unwrap_or_else(|err| panic!(f!"{err:?}", -222));


    match <[Record; 1]>::try_from(records) {
        Ok([Record { id, ip, name}]) => {
            assert!(name == RECORD, f!"Expected {RECORD} found {name}", 99);

            let current_ip = ip_task.await
                .unwrap_or_else(|err| panic!(f!"Join Error: {err:?}", -333));

            if current_ip == ip { exit(0) }


            let response = retry!(retry!(update_record(&client, &id, &current_ip))).await;

            if let Err(err) = response {
                let string = String::from_utf8_lossy(err.as_ref());

                let path = std::env::temp_dir()
                    .join(Uuid::new_v4().to_string())
                    .with_extension("log");

                tokio::fs::write(&path, string.as_ref()).await
                    .unwrap_or_else(|err| panic!(f!"FATAL WRITE ERR: {err:?}", -666));

                Command::new("notepad").arg(&path).spawn()
                    .unwrap_or_else(|err| panic!(f!"FATAL SPAWN ERR: {err:?}", -777))
                    .wait().await
                    .unwrap_or_else(|err| panic!(f!"FATAL SPAWN AWAIT ERR: {err:?}", -888));

                let _ = tokio::fs::remove_file(&path).await;
            }
        }
        Err(res) => {
            let len = res.len();
            panic!(f!"Expected 1 record Got {len}", 99);
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let msg = match info.payload().downcast_ref::<&str>() {
            Some(&s) => s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => s.as_str(),
                None => "Box<dyn Any>",
            },
        };

        exit_err(msg, -0xFF)
    }));

    real_main()
}