#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

extern crate core;

use crate::addr_helper::{IpType, IpUpdateType};
use crate::config::Config;
use crate::config::ip_source::{GetIpError, IpSource};
use crate::json::EscapeExt;
use crate::one_or_more::OneOrMore;
use crate::retrying_client::RetryingClient;
use crate::time::new_skip_interval;
use crate::updaters::{UpdaterEvent, UpdaterExitStatus};
use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use futures::future::Either;
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::borrow::Cow;
use std::cell::Cell;
use std::fmt::Display;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::NonZeroU8;
use std::panic::AssertUnwindSafe;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::thread::Builder;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::{join, try_join};

mod addr_helper;
mod config;
mod console_listener;
mod err;
mod global_rt;
mod json;
mod network;
mod num_cpus;
mod one_or_more;
mod pre;
mod retrying_client;
mod time;
mod updaters;

struct DDNSContext {
    client: RetryingClient,
    user_messages: UserMessages,
}

#[derive(Debug)]
struct Record<T> {
    id: Box<str>,
    ip: T,
}

type RecordV4 = Record<Ipv4Addr>;
type RecordV6 = Record<Ipv6Addr>;

impl DDNSContext {
    fn new(cfg: Config) -> Self {
        DDNSContext {
            client: RetryingClient::new(&cfg),
            user_messages: UserMessages::new(cfg.misc().general().max_errors()),
        }
    }

    async fn get_ips(&self, cfg: &Config) -> Result<(Option<Ipv4Addr>, Option<Ipv6Addr>)> {
        struct FlatVec<T>(Vec<T>);

        impl<T> Default for FlatVec<T> {
            fn default() -> Self {
                Self(vec![])
            }
        }

        impl<T> Extend<Option<T>> for FlatVec<T> {
            fn extend<I: IntoIterator<Item = Option<T>>>(&mut self, iter: I) {
                self.0.extend(iter.into_iter().flatten())
            }
        }

        fn make_specific_stream<T, S: Stream<Item = T>, F: FnOnce() -> S>(
            make_stream: F,
            need_stream: bool,
        ) -> (Option<futures::stream::AbortHandle>, impl Stream<Item = T>) {
            match need_stream {
                true => {
                    let (abort_handle, abort_registration) =
                        futures::stream::AbortHandle::new_pair();

                    let stream = make_stream();
                    let abortable = futures::stream::Abortable::new(stream, abort_registration);

                    (Some(abort_handle), Either::Right(abortable))
                }

                false => (None, Either::Left(futures::stream::empty())),
            }
        }

        let (any_ip, ipv6, ipv4) = cfg
            .ip_sources()
            .map(|source| match source.ip_type() {
                IpType::Any => (Some(source), None, None),
                IpType::V6 => (None, Some(source), None),
                IpType::V4 => (None, None, Some(source)),
            })
            .collect::<(FlatVec<_>, FlatVec<_>, FlatVec<_>)>();

        let last_err = Cell::new(None);
        let make_stream = |sources: FlatVec<IpSource>| {
            let iter = sources
                .0
                .into_iter()
                .map(|source| source.resolve_ip(&self.client, cfg));

            futures::stream::iter(iter)
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
                })
        };

        let update_type = cfg.zone().ip_update_type();

        let (abort_ipv4, ipv4_stream) = make_specific_stream(
            || make_stream(ipv4),
            !matches!(update_type, IpUpdateType::V6),
        );

        // IPv6 gets special handling for 2 reasons:
        // 1. IPv6 privacy addresses are used by default on most OSes, it works but is less useful
        // 2. The device usually knows its own IPv6 unicast global address, no need to ask a server for it
        let (abort_ipv6, ipv6_stream) = make_specific_stream(
            || {
                let stream = futures::stream::once(async {
                    let try_native = network::native_get_ipv6_addr()
                        .await
                        .map_err(GetIpError::NativeIpv6GrabError);

                    match try_native {
                        Ok(Some(ip)) => {
                            let ip = IpAddr::V6(ip);
                            return Either::Right(futures::stream::iter([ip]));
                        }
                        Ok(None) => {}
                        Err(error) => last_err.set(Some(error)),
                    }

                    Either::Left(make_stream(ipv6))
                });

                stream.flatten()
            },
            !matches!(update_type, IpUpdateType::V4),
        );

        let any_stream = make_stream(any_ip);

        let (ipv4, ipv6) = {
            let some_specific_ip = futures::stream::select(ipv4_stream, ipv6_stream);
            let mut stream = std::pin::pin!(futures::stream::select(some_specific_ip, any_stream));

            let mut ipv4 = Err(abort_ipv4);
            let mut ipv6 = Err(abort_ipv6);

            while let Some(addr) = stream.next().await {
                match addr {
                    IpAddr::V4(addr) if let Err(abort) = ipv4 => {
                        ipv4 = Ok(addr);
                        if let Some(abort) = abort {
                            abort.abort()
                        }
                    }
                    IpAddr::V6(addr) if let Err(abort) = ipv6 => {
                        ipv6 = Ok(addr);
                        if let Some(abort) = abort {
                            abort.abort()
                        }
                    }
                    // take only the first IP
                    // filter the rest
                    IpAddr::V4(_) | IpAddr::V6(_) => {}
                }

                let exit_cond = match update_type {
                    IpUpdateType::Any => ipv4.is_ok() || ipv6.is_ok(),
                    IpUpdateType::Both => ipv4.is_ok() && ipv6.is_ok(),
                    IpUpdateType::V6 => ipv4.is_ok(),
                    IpUpdateType::V4 => ipv6.is_ok(),
                };

                if exit_cond {
                    break;
                }
            }

            (ipv4.ok(), ipv6.ok())
        };

        let error = match update_type {
            IpUpdateType::Any => ipv4.is_none() && ipv6.is_none(),
            IpUpdateType::Both => ipv4.is_none() || ipv6.is_none(),
            IpUpdateType::V6 => ipv6.is_none(),
            IpUpdateType::V4 => ipv4.is_none(),
        };

        if error {
            bail!(last_err.into_inner().unwrap_or(GetIpError::NoIpSources))
        }

        Ok((ipv4, ipv6))
    }

    async fn get_records(&self, cfg: &Config) -> Result<(Option<RecordV4>, Option<RecordV6>)> {
        #[derive(Debug, Deserialize)]
        struct FullIpTypeRecord<T> {
            id: Box<str>,
            name: Box<str>,
            #[serde(rename = "content")]
            ip: T,
        }

        #[derive(Debug, Deserialize)]
        pub struct GetResponse<T> {
            result: OneOrMore<FullIpTypeRecord<T>>,
        }

        async fn get_bytes(this: &DDNSContext, cfg: &Config, record_type: &str) -> Result<Bytes> {
            let url = format!(
                "https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records?type={record_type}&name={record}",
                zone_id = cfg.zone().id(),
                record = cfg.zone().record()
            );

            cfg.authorize_request(this.client.get(url))
                .send()
                .await?
                .bytes()
                .await
                .map_err(anyhow::Error::new)
        }

        trait IpVersion: DeserializeOwned {
            const RECORD: &str;
        }

        impl IpVersion for Ipv4Addr {
            const RECORD: &str = "A";
        }

        impl IpVersion for Ipv6Addr {
            const RECORD: &str = "AAAA";
        }

        async fn get_record_typed<T: IpVersion>(
            this: &DDNSContext,
            cfg: &Config,
        ) -> Result<Record<T>> {
            let response = get_bytes(this, cfg, T::RECORD).await?;
            let FullIpTypeRecord { name, id, ip } =
                match serde_json::from_slice::<GetResponse<T>>(&response)?.result {
                    OneOrMore::Zero => {
                        bail!("found zero corresponding records {}", cfg.zone().record())
                    }
                    OneOrMore::More => bail!(
                        "found too many corresponding `{TYPE}` records for {}",
                        cfg.zone().record(),
                        TYPE = T::RECORD,
                    ),
                    OneOrMore::One(record) => record,
                };

            anyhow::ensure!(
                &*name == cfg.zone().record(),
                "Expected {} found {name}",
                cfg.zone().record()
            );

            Ok(Record { id, ip })
        }

        Ok(match cfg.zone().ip_update_type() {
            IpUpdateType::Any => {
                let (v4, v6) = join!(
                    get_record_typed::<Ipv4Addr>(self, cfg),
                    get_record_typed::<Ipv6Addr>(self, cfg)
                );
                match (v4, v6) {
                    (Err(v4_err), Err(v6_err)) => bail!(v4_err.context(v6_err)),
                    (v4, v6) => (v4.ok(), v6.ok()),
                }
            }
            IpUpdateType::Both => {
                let (v4, v6) = try_join!(
                    get_record_typed::<Ipv4Addr>(self, cfg),
                    get_record_typed::<Ipv6Addr>(self, cfg)
                )?;
                (Some(v4), Some(v6))
            }
            IpUpdateType::V6 => (None, Some(get_record_typed::<Ipv6Addr>(self, cfg).await?)),
            IpUpdateType::V4 => (Some(get_record_typed::<Ipv4Addr>(self, cfg).await?), None),
        })
    }

    async fn update_record(&self, id: &str, ip: IpAddr, cfg: &Config) -> Result<()> {
        let request_json = format! {
            r###"{{"type":"{record_type}","name":"{record}","content":"{ip}","proxied":{proxied}}}"###,
            record_type = match ip {
                IpAddr::V4(_) => "A",
                IpAddr::V6(_) => "AAAA"
            },
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
            bail!("Bad response: {}", String::from_utf8_lossy(&bytes))
        }

        Ok(())
    }

    pub async fn run_ddns(&self, cfg: Config) -> Result<impl Display> {
        let ((v4_record, v6_record), (v4_ip, v6_ip)) =
            try_join!(self.get_records(&cfg), self.get_ips(&cfg))?;

        let has_ip_source = match (v4_record.as_ref(), v6_record.as_ref()) {
            (Some(_), Some(_)) => v4_ip.is_some() || v6_ip.is_some(),
            (Some(_), None) => v4_ip.is_some(),
            (None, Some(_)) => v6_ip.is_some(),
            (None, None) => {
                unreachable!("two empty records didn't cause an error")
            }
        };

        ensure!(has_ip_source, GetIpError::NoIpSources);

        enum UpdateStatus {
            SameIp,
            Updated,
            NoRecordExists,
        }

        impl UpdateStatus {
            pub fn msg(self) -> &'static str {
                match self {
                    UpdateStatus::SameIp => "IP didn't change, skipped record update.",
                    UpdateStatus::Updated => "Successfully Updated!",
                    UpdateStatus::NoRecordExists => "No Record exists in cloudflare.",
                }
            }
        }

        async fn update<T: Into<IpAddr> + Eq>(
            this: &DDNSContext,
            cfg: &Config,
            record: Option<Record<T>>,
            ip: Option<T>,
        ) -> Result<UpdateStatus> {
            if let Some(record) = record
                && let Some(ip) = ip
            {
                if record.ip == ip {
                    return Ok(UpdateStatus::SameIp);
                }

                this.update_record(&record.id, ip.into(), cfg).await?;
                return Ok(UpdateStatus::Updated);
            }

            Ok(UpdateStatus::NoRecordExists)
        }

        let (changed_v4, changed_v6) = join!(
            update(self, &cfg, v4_record, v4_ip),
            update(self, &cfg, v6_record, v6_ip),
        );

        match (changed_v4, changed_v6) {
            (Err(err), Ok(UpdateStatus::NoRecordExists))
            | (Ok(UpdateStatus::NoRecordExists), Err(err)) => Err(err),

            (Err(v4), Err(v6)) => Err(v4.context(v6)),

            (Ok(UpdateStatus::NoRecordExists), Ok(UpdateStatus::NoRecordExists)) => {
                unreachable!("at least one record exists")
            }

            (Ok(status), Ok(UpdateStatus::NoRecordExists))
            | (Ok(UpdateStatus::NoRecordExists), Ok(status))
                if let ty = cfg.zone().ip_update_type()
                    && !matches!(ty, IpUpdateType::Any) =>
            {
                assert!(
                    !matches!(ty, IpUpdateType::Both),
                    "BUG: requested both ip types; found one and try to continue updating only one"
                );

                Ok(Cow::Borrowed(status.msg()))
            }

            (status_v4 @ Ok(_), status_v6 @ Ok(_))
            | (status_v4 @ Ok(_), status_v6 @ Err(_))
            | (status_v4 @ Err(_), status_v6 @ Ok(_)) => {
                let display = |res: Result<UpdateStatus>| {
                    res.map_or_else(
                        |err| Cow::Owned(format!("Failed: \n{err}\n")),
                        |status| Cow::Borrowed(status.msg()),
                    )
                };

                let msg = format!("IPv4: {} IPv6: {}", display(status_v4), display(status_v6));

                Ok(Cow::Owned(msg))
            }
        }
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
        network::subscribe_native_changes(&mut updaters_manager)?;
    }

    err::exit::subscribe(&mut updaters_manager)?;
    console_listener::subscribe(&mut updaters_manager)?;

    let mut interval = new_skip_interval(cfg_store.load_config().misc().refresh().interval());

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if !network::has_internet().await {
                    dbg_println!("no internet available skipping update");
                    continue;
                }

                dbg_println!("updating");
                match ctx.run_ddns(cfg_store.load_config()).await {
                    Err(err) => ctx.user_messages.error(err.to_string()).await,
                    Ok(debug_message) => dbg_println!("{}", debug_message),
                }
            },

            res = updaters_manager.watch() => match res {
                UpdaterEvent::Update => interval.reset_immediately(),
                UpdaterEvent::ServiceEvent(exit) => {
                    match *exit.status() {
                        UpdaterExitStatus::Success => {},
                        UpdaterExitStatus::Panic
                        | UpdaterExitStatus::Error(_) => {
                            ctx.user_messages
                                .error(format!("Updater abruptly exited: {exit}")).await
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
        .worker_threads(num_cpus::num_cpus().get())
        .enable_all()
        .build()
        .expect("failed to build runtime")
}

fn main() -> ExitCode {
    pre::pre_run();
    #[cfg(feature = "trace")]
    console_subscriber::init();

    const MAX_PANIC_COUNT: u32 = 8;

    let mut runtime = make_runtime();
    let mut panic_count = 0;
    loop {
        let exit = std::panic::catch_unwind(AssertUnwindSafe(|| runtime.block_on(real_main())));

        if exit.is_ok() {
            panic_count = 0;
        }

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
                // best effort cleanup
                let _ = Builder::new().spawn(move || drop(runtime));
                abort!("{e}")
            }

            // Recoverable
            Ok(Ok(Action::Restart)) => dbg_println!("Restarting..."),
            Err(panic) => {
                if panic_count >= MAX_PANIC_COUNT {
                    std::panic::resume_unwind(panic)
                }

                panic_count = panic_count.strict_add(1);

                // the old runtime might be in an invalid state.
                // replace it and drop it on a new thread to avoid hanging
                let old_runtime = std::mem::replace(&mut runtime, make_runtime());
                thread::spawn(move || drop(old_runtime));

                dbg_println!("Panicked!!");
                dbg_println!("Restarting in 15s...");
                thread::sleep(Duration::from_secs(15));
                dbg_println!("Restarting")
            }
        }
    }
}
