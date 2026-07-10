#![cfg(windows)]

// huge thx to
// https://github.com/suryatmodulus/firezone/blob/7c296494bd96c34ef1c0be75285ff92566f4c12c/rust/gui-client/src-tauri/src/client/network_changes.rs

use crate::updaters::Updater;
use crate::{abort_unreachable, dbg_println};
use anyhow::Context;
use std::marker::{PhantomData, PhantomPinned};
use std::mem::MaybeUninit;
use std::net::Ipv6Addr;
use std::pin::Pin;
use std::ptr::NonNull;
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_FRIENDLY_NAME,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::NetworkListManager::{
    INetworkEvents, INetworkEvents_Impl, INetworkListManager, NLM_CONNECTIVITY,
    NLM_CONNECTIVITY_IPV4_INTERNET, NLM_CONNECTIVITY_IPV6_INTERNET, NLM_NETWORK_PROPERTY_CHANGE,
    NetworkListManager,
};
use windows::Win32::Networking::WinSock::{AF_INET6, IpSuffixOriginRandom, SOCKADDR_IN6};
use windows::Win32::System::Com;
use windows::core::Result as WinResult;
use windows::core::{GUID, Interface, implement};

#[derive(thiserror::Error, Debug)]
pub enum UpdaterError {
    #[error("Couldn't initialize COM: {0}")]
    ComInitialize(windows::core::Error),
    #[error("Couldn't create NetworkListManager")]
    CreateNetworkListManager(windows::core::Error),
    #[error("Couldn't start listening to network events: {0}")]
    Listening(windows::core::Error),
    #[error("Couldn't stop listening to network events: {0}")]
    Unadvise(windows::core::Error),
}

#[derive(Copy, Clone)]
struct Permit<'a>(PhantomData<Pin<&'a ComGuard>>);

#[clippy::has_significant_drop]
struct ComGuard {
    _pinned: PhantomPinned,
    _unsend_unsync: PhantomData<*const ()>,
}

impl ComGuard {
    pub fn new() -> Result<Self, UpdaterError> {
        unsafe { Com::CoInitializeEx(None, Com::COINIT_MULTITHREADED) }
            .ok()
            .map_err(UpdaterError::ComInitialize)?;
        Ok(Self {
            _pinned: PhantomPinned,
            _unsend_unsync: PhantomData,
        })
    }

    #[inline(always)]
    pub const fn get_permit(self: Pin<&Self>) -> Permit<'_> {
        Permit(PhantomData)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { Com::CoUninitialize() };
    }
}

struct UpdateManager<'a> {
    advise_cookie_net: Option<u32>,
    cxn_point_net: Com::IConnectionPoint,
    inner: UpdaterInner<'a>,
    _permit: Permit<'a>,
}

impl<'a> Drop for UpdateManager<'a> {
    fn drop(&mut self) {
        if let Some(cookie) = self.advise_cookie_net.take() {
            unsafe { self.cxn_point_net.Unadvise(cookie) }
                .map_err(UpdaterError::Unadvise)
                .unwrap();
        }
    }
}

macro_rules! unwrap_win32 {
    ($x: expr) => {
        $x.unwrap_or_else(|err| abort_unreachable!("Fatal win32 api error {err}"))
    };
}

thread_local! {
    static COM_GUARD: Pin<Box<ComGuard>> = unwrap_win32!(ComGuard::new().map(Box::pin));

    static NETWORK_MANGER: INetworkListManager = {
        let res = COM_GUARD.with(|x| {
            let _permit = x.as_ref().get_permit();
            unsafe { Com::CoCreateInstance(&NetworkListManager, None, Com::CLSCTX_ALL) }
                .map_err(UpdaterError::CreateNetworkListManager)
        });

        unwrap_win32!(res)
    }
}

pub async fn has_internet() -> bool {
    fn inner() -> Result<bool, ()> {
        NETWORK_MANGER.with(|network_manager| {
            match unsafe { network_manager.IsConnectedToInternet() } {
                Ok(connected) => Ok(connected.as_bool()),
                Err(_) => Err(()),
            }
        })
    }

    match tokio::task::spawn_blocking(inner).await {
        Ok(Ok(x)) => x,
        _ => super::fallback_has_internet().await,
    }
}

fn listen<F: Fn(), S: Fn() -> T, T>(notify_callback: F, shutdown: S) -> Result<T, UpdaterError> {
    COM_GUARD.with(move |com_guard| {
        let _permit = Pin::as_ref(com_guard).get_permit();

        let cxn_point_net = NETWORK_MANGER.with(|network_list_manager| {
            let cpc: Com::IConnectionPointContainer = network_list_manager
                .cast()
                .map_err(UpdaterError::Listening)?;

            unsafe { cpc.FindConnectionPoint(&INetworkEvents::IID) }
                .map_err(UpdaterError::Listening)
        })?;

        let mut this = UpdateManager {
            advise_cookie_net: None,
            cxn_point_net,
            inner: UpdaterInner {
                notify_callback: &notify_callback,
                _permit,
            },
            _permit,
        };

        let callbacks: INetworkEvents = this.inner.into();

        this.advise_cookie_net = Some(
            unsafe { this.cxn_point_net.Advise(&callbacks) }.map_err(UpdaterError::Listening)?,
        );

        Ok(shutdown())
    })
}

pub fn subscribe(updater: Updater) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let local_notify = Notify::new();

        let notify_callback = || {
            dbg_println!("Network Listener: got network update!");
            if updater.update().is_err() {
                local_notify.notify_waiters()
            }
        };

        let shutdown = || {
            TokioHandle::current().block_on(async {
                tokio::select! {
                    _ = local_notify.notified() => (),
                    _ = updater.wait_shutdown() => ()
                }
            })
        };

        let res = listen(notify_callback, shutdown);
        updater.exit(res)
    })
}

#[implement(INetworkEvents)]
#[derive(Copy, Clone)]
struct UpdaterInner<'a> {
    notify_callback: &'a dyn Fn(),
    _permit: Permit<'a>,
}

#[allow(non_snake_case)]
impl<'a> INetworkEvents_Impl for UpdaterInner_Impl<'a> {
    fn NetworkAdded(&self, _: &GUID) -> WinResult<()> {
        Ok(())
    }

    fn NetworkDeleted(&self, _: &GUID) -> WinResult<()> {
        Ok(())
    }

    fn NetworkConnectivityChanged(
        &self,
        _: &GUID,
        new_connectivity: NLM_CONNECTIVITY,
    ) -> WinResult<()> {
        const HAS_INTERNET_MASK: i32 =
            NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0;

        let has_internet = (new_connectivity.0 & HAS_INTERNET_MASK) != 0;
        if has_internet {
            (self.notify_callback)()
        }

        Ok(())
    }

    fn NetworkPropertyChanged(&self, _: &GUID, _: NLM_NETWORK_PROPERTY_CHANGE) -> WinResult<()> {
        Ok(())
    }
}

fn get_ipv6_addr_sync() -> anyhow::Result<Option<Ipv6Addr>> {
    // Microsoft's recommended starting size to avoid the overflow/retry dance in
    // the common case (see "Remarks" on the GetAdaptersAddresses docs page).
    const WORKING_BUFFER_SIZE: usize = 32 * 1024;
    const MAX_TRIES: u8 = 5;

    let flags = GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER
        | GAA_FLAG_SKIP_FRIENDLY_NAME;

    let cheap_buffer_size = u32::try_from(WORKING_BUFFER_SIZE).unwrap_or(u32::MAX);

    let mut buf_len = cheap_buffer_size;

    #[repr(C, align(16))]
    struct AlignedWord(u128);

    const NUMBER_ELEMENTS_STACK_BUF: usize =
        { WORKING_BUFFER_SIZE.div_ceil(size_of::<AlignedWord>()) };

    let mut heap_buffer: Vec<AlignedWord> = Vec::new();
    let mut stack_buffer: [MaybeUninit<AlignedWord>; NUMBER_ELEMENTS_STACK_BUF] =
        [const { MaybeUninit::uninit() }; NUMBER_ELEMENTS_STACK_BUF];

    let mut tries = 0;

    let adapters_ptr: *mut IP_ADAPTER_ADDRESSES_LH = loop {
        let buffer = match buf_len <= cheap_buffer_size {
            true => stack_buffer.as_mut_ptr(),
            false => {
                heap_buffer.clear();
                let word_count = buf_len.div_ceil(
                    const {
                        // FIXME(const convert)
                        let size_of_word = size_of::<AlignedWord>();
                        assert!(size_of_word < 256);
                        size_of_word as u32
                    },
                );
                let new_capacity = usize::try_from(word_count).unwrap_or(usize::MAX);
                heap_buffer
                    .try_reserve_exact(new_capacity)
                    .context("could not get ipv6 method in a native way, because of OOM")?;

                heap_buffer.spare_capacity_mut().as_mut_ptr()
            }
        };

        let ptr = buffer.cast::<IP_ADAPTER_ADDRESSES_LH>();

        // SAFETY: `ptr` points at a `buf_len`-byte buffer we just allocated;
        // GetAdaptersAddresses will only write within that bound and updates
        // `buf_len` in place if it needs more room.
        let ret = unsafe {
            GetAdaptersAddresses(AF_INET6.0.into(), flags, None, Some(ptr), &mut buf_len)
        };

        if ret == ERROR_SUCCESS.0 {
            break ptr;
        } else if ret == ERROR_BUFFER_OVERFLOW.0 && tries < MAX_TRIES {
            tries = tries.strict_add(1);
            // buf_len now holds the required size, loop and reallocate
            continue;
        } else {
            anyhow::bail!(
                "`GetAdaptersAddresses` failed with error: {}",
                std::io::Error::from_raw_os_error(ret.cast_signed())
            );
        }
    };

    let mut candidate: Option<Ipv6Addr> = None;
    let mut adapter = adapters_ptr;

    // SAFETY: `adapter` is either the head returned above, or `.Next` from
    // a previously validated node in the linked list GetAdaptersAddresses gave us.
    'candidate_search: while let Some(adapter_ref) = unsafe { adapter.as_ref() } {
        let mut unicast = adapter_ref.FirstUnicastAddress;

        // SAFETY: same reasoning here; walking a linked list owned by `buffer`.
        while let Some(ua) = unsafe { unicast.as_ref() } {
            if let Some(sockaddr) = NonNull::new(ua.Address.lpSockaddr) {
                // SAFETY: lpSockaddr is non-null and, for AF_INET6 entries,
                // points at a SOCKADDR_IN6-sized structure per the WinSock contract.
                let family = unsafe { (*sockaddr.as_ptr()).sa_family };
                if family == AF_INET6 {
                    let sin6 = unsafe { sockaddr.cast::<SOCKADDR_IN6>().as_ref() };
                    // SAFETY: reading a plain [u8; 16] union field - no invalid
                    // bit patterns possible for this type.
                    let octets = unsafe { sin6.sin6_addr.u.Byte };
                    let ip = Ipv6Addr::from_octets(octets);

                    let valid_global = !ip.is_loopback()
                        && !ip.is_unicast_link_local()
                        && !ip.is_unique_local()
                        && !ip.is_multicast();

                    if valid_global {
                        let temporary = ua.SuffixOrigin == IpSuffixOriginRandom;
                        if !temporary {
                            candidate = Some(ip);
                            break 'candidate_search;
                        }

                        candidate = candidate.or(Some(ip));
                    }
                }
            }

            unicast = ua.Next;
        }

        adapter = adapter_ref.Next;
    }

    Ok(candidate)
}

pub async fn native_get_ipv6_addr() -> anyhow::Result<Option<Ipv6Addr>> {
    tokio::task::spawn_blocking(get_ipv6_addr_sync)
        .await
        .context("failed to spawn ipv6 native addr grab task")?
}
