#![cfg(windows)]

use windows::core::{BSTR, GUID, HSTRING, PCWSTR};
use windows::Win32::System::Com::{DISPATCH_FLAGS, DISPPARAMS, EXCEPINFO, IDispatch_Impl, ITypeInfo};
use windows::Win32::System::EventNotificationService::{ISensNetwork_Impl, ISensNetwork, SENS_QOCINFO, SENS_CONNECTION_TYPE};
use windows::Win32::System::Variant::VARIANT;
use crate::wide_str;

#[derive(Copy, Clone)]
pub struct UpdateDns;


#[windows::core::implement(ISensNetwork)]
pub struct UpdateWatcher(tokio::sync::watch::Sender<UpdateDns>);


#[allow(non_snake_case)]
impl IDispatch_Impl for UpdateWatcher {
    fn GetTypeInfoCount(&self) -> windows::core::Result<u32> {
        Ok(0)
    }
    fn GetTypeInfo(&self, _: u32, _: u32) -> windows::core::Result<ITypeInfo> {
        Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            HSTRING::from_wide(wide_str!(wide; "GetTypeInfo Error \t\n\r"))
                .unwrap(),
        ))
    }

    fn GetIDsOfNames(&self, _: *const GUID, _: *const PCWSTR, _: u32, _: u32, _: *mut i32) -> windows::core::Result<()> {
        Ok(())
    }

    fn Invoke(
        &self,
        _dispidmember: i32,
        _riid: *const GUID,
        _lcid: u32,
        _wflags: DISPATCH_FLAGS,
        _pdispparams: *const DISPPARAMS,
        _pvarresult: *mut VARIANT,
        _pexcepinfo: *mut EXCEPINFO,
        _puargerr: *mut u32
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

#[allow(non_snake_case)]
impl ISensNetwork_Impl for UpdateWatcher {
    fn ConnectionMade(&self, bstrconnection: &BSTR, ultype: u32, lpqocinfo: *const SENS_QOCINFO) -> windows::core::Result<()> {
        todo!()
    }

    fn ConnectionMadeNoQOCInfo(&self, bstrconnection: &BSTR, ultype: u32) -> windows::core::Result<()> {
        todo!()
    }

    fn ConnectionLost(&self, bstrconnection: &BSTR, ultype: SENS_CONNECTION_TYPE) -> windows::core::Result<()> {
        todo!()
    }

    fn DestinationReachable(&self, bstrdestination: &BSTR, bstrconnection: &BSTR, ultype: u32, lpqocinfo: *const SENS_QOCINFO) -> windows::core::Result<()> {
        todo!()
    }

    fn DestinationReachableNoQOCInfo(&self, bstrdestination: &BSTR, bstrconnection: &BSTR, ultype: u32) -> windows::core::Result<()> {
        todo!()
    }
}