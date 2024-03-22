#![cfg(windows)]

// huge thx to
// https://github.com/suryatmodulus/firezone/blob/7c296494bd96c34ef1c0be75285ff92566f4c12c/rust/gui-client/src-tauri/src/client/network_changes.rs

use std::convert::Infallible;
use std::marker::{PhantomData, PhantomPinned};
use std::pin::{Pin, pin};
use std::thread;
use windows::core::{GUID, implement, Interface};
use windows::Win32::System::Com;
use windows::Win32::Networking::NetworkListManager::{INetworkEvents, INetworkEvents_Impl, INetworkListManager, NetworkListManager, NLM_CONNECTIVITY, NLM_CONNECTIVITY_IPV4_INTERNET, NLM_CONNECTIVITY_IPV6_INTERNET, NLM_NETWORK_PROPERTY_CHANGE};
use windows::core::Result as WinResult;
use windows::Win32::Foundation::VARIANT_BOOL;
use crate::updaters::{Updater, UpdatersManager};

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

#[clippy::has_significant_drop]
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


struct UpdateManager<'a> {
    advise_cookie_net: Option<u32>,
    cxn_point_net: Com::IConnectionPoint,
    _permit: Permit<'a>
}

impl<'a> Drop for UpdateManager<'a> {
    fn drop(&mut self) {
        if let Some(cookie) = self.advise_cookie_net.take() {
            unsafe { self.cxn_point_net.Unadvise(cookie) }.map_err(UpdaterError::Unadvise).unwrap();
        }
    }
}

pub fn subscribe(updaters_manager: &mut UpdatersManager) {
    let updater: Updater = updaters_manager.add_updater("network-listener");
    tokio::task::spawn_blocking(move || {
        #[inline(always)]
        fn inner(notify_callback: &dyn Fn()) -> Result<Infallible, UpdaterError> {
            let com_guard = pin!(ComGuard::new()?);
            let com_guard = com_guard.as_ref();

            let network_list_manager: INetworkListManager =
                unsafe { Com::CoCreateInstance(&NetworkListManager, None, Com::CLSCTX_ALL) }
                    .map_err(UpdaterError::CreateNetworkListManager)?;
            let cpc: Com::IConnectionPointContainer =
                network_list_manager.cast().map_err(UpdaterError::Listening)?;
            let cxn_point_net =
                unsafe { cpc.FindConnectionPoint(&INetworkEvents::IID) }.map_err(UpdaterError::Listening)?;

            let mut this = UpdateManager {
                advise_cookie_net: None,
                cxn_point_net,
                _permit: com_guard.get_permit(),
            };

            let callbacks: INetworkEvents = UpdaterInner { 
                notify_callback,
                _permit: com_guard.get_permit()
            }.into();
            
            this.advise_cookie_net = Some(
                unsafe { this.cxn_point_net.Advise(&callbacks) }.map_err(UpdaterError::Listening)?
            );
            
            loop { thread::park() }
        }
        
        match inner(&|| updater.update()) {
            Ok(x) => match x {  }
            Err(err) => updater.shutdown(err)
        }
    });
}

pub fn has_internet() -> bool {
    thread_local! {
        static NETWORK_MANGER: INetworkListManager = 
            unsafe { Com::CoCreateInstance(&NetworkListManager, None, Com::CLSCTX_ALL) }
                .expect("Unable to get an instance of INetworkListManager");
    }
    
    NETWORK_MANGER.with(|x| unsafe { x.IsConnectedToInternet() })
        .map(VARIANT_BOOL::as_bool)
        .unwrap_or(false)
}

#[implement(INetworkEvents)]
struct UpdaterInner<'a> {
    notify_callback: &'a dyn Fn(),
    _permit: Permit<'a>
}

#[allow(non_snake_case)]
impl<'a> INetworkEvents_Impl for UpdaterInner<'a> {
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
        if has_internet { (self.notify_callback)() }
        
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