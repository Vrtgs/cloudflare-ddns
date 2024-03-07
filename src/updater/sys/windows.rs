#![cfg(windows)]

// huge thx to
// https://github.com/suryatmodulus/firezone/blob/7c296494bd96c34ef1c0be75285ff92566f4c12c/rust/gui-client/src-tauri/src/client/network_changes.rs

use std::convert::Infallible;
use std::marker::{PhantomData, PhantomPinned};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::thread;
use tokio::sync::Notify;
use tokio::sync::oneshot::Sender as OnsShotSender;
use windows::core::{GUID, implement, Interface};
use windows::Win32::System::Com;
use windows::Win32::Networking::NetworkListManager::{INetworkEvents, INetworkEvents_Impl, INetworkListManager, NetworkListManager, NLM_CONNECTIVITY, NLM_CONNECTIVITY_IPV4_INTERNET, NLM_CONNECTIVITY_IPV6_INTERNET, NLM_NETWORK_PROPERTY_CHANGE};
use windows::core::Result as WinResult;

#[derive(thiserror::Error, Debug)]
pub enum UpdaterError {
    #[error("Couldn't initialize COM: {0}")]
    ComInitialize(windows::core::Error),
    #[error("Couldn't create NetworkListManager")]
    CreateNetworkListManager(windows::core::Error),
    #[error("Couldn't start listening to network events: {0}")]
    Listening(windows::core::Error),
    #[error("Couldn't stop listening to network events: {0}")]
    Unadvise(windows::core::Error)
}


struct Permit<'a>(PhantomData<Pin<&'a ComGuard>>);

struct ComGuard {
    _pinned: PhantomPinned,
    _unsend_unsync: PhantomData<*const ()>
}

impl ComGuard {
    pub fn new() -> Result<Self, UpdaterError> {
        unsafe { Com::CoInitializeEx(None, Com::COINIT_MULTITHREADED) }
            .ok().map_err(UpdaterError::ComInitialize)?;
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


struct InternetUpdateWatcher<'a> {
    advise_cookie_net: Option<u32>,
    cxn_point_net: Com::IConnectionPoint,
    inner: UpdaterInner,
    _permit: Permit<'a>
}

impl<'a> Drop for InternetUpdateWatcher<'a> {
    fn drop(&mut self) {
        if let Some(cookie) = self.advise_cookie_net.take() {
            unsafe { self.cxn_point_net.Unadvise(cookie) }.map_err(UpdaterError::Unadvise).unwrap();
        }
    }
}

pub fn subscribe(notify: Arc<Notify>, shutdown_sender: OnsShotSender<UpdaterError>) {
    tokio::task::spawn_blocking(move || {
        #[inline(always)]
        fn inner(notify: Arc<Notify>) -> Result<Infallible, UpdaterError> {
            let com_guard = ComGuard::new()?;
            let com_guard = pin!(com_guard);

            let network_list_manager: INetworkListManager =
                unsafe { Com::CoCreateInstance(&NetworkListManager, None, Com::CLSCTX_ALL) }
                    .map_err(UpdaterError::CreateNetworkListManager)?;
            let cpc: Com::IConnectionPointContainer =
                network_list_manager.cast().map_err(UpdaterError::Listening)?;
            let cxn_point_net =
                unsafe { cpc.FindConnectionPoint(&INetworkEvents::IID) }.map_err(UpdaterError::Listening)?;

            let mut this = InternetUpdateWatcher {
                advise_cookie_net: None,
                cxn_point_net,
                inner: UpdaterInner { notify },
                _permit: com_guard.as_ref().get_permit(),
            };

            let callbacks: INetworkEvents = this.inner.clone().into();

            this.advise_cookie_net = Some(
                unsafe { this.cxn_point_net.Advise(&callbacks) }.map_err(UpdaterError::Listening)?
            );
            
            loop { thread::park() }
        }
        
        match inner(notify) {
            Ok(x) => match x {  }
            Err(err) => shutdown_sender.send(err)
        }
    });
}


#[implement(INetworkEvents)]
#[derive(Clone)]
struct UpdaterInner {
    notify: Arc<Notify>,
}

#[allow(non_snake_case)]
impl INetworkEvents_Impl for UpdaterInner {
    fn NetworkAdded(&self, _: &GUID) -> WinResult<()> {
        Ok(())
    }

    fn NetworkDeleted(&self, _: &GUID) -> WinResult<()> {
        Ok(())
    }

    fn NetworkConnectivityChanged(&self, _: &GUID, new_connectivity: NLM_CONNECTIVITY) -> WinResult<()> {
        const HAS_INTERNET_MASK: i32 = 
            NLM_CONNECTIVITY_IPV4_INTERNET.0
            | NLM_CONNECTIVITY_IPV6_INTERNET.0;
        
        let has_internet = (new_connectivity.0 & HAS_INTERNET_MASK) != 0;
        if has_internet { 
            self.notify.notify_waiters();
        }
        Ok(())
    }

    fn NetworkPropertyChanged(
        &self,
        _: &GUID,
        _: NLM_NETWORK_PROPERTY_CHANGE,
    ) -> WinResult<()> {
        Ok(())
    }
}