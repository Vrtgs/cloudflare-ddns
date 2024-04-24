use std::borrow::Cow;
use std::panic::PanicInfo;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::Semaphore;

pub mod exit;

#[macro_export]
macro_rules! dbg_println {
    ($($arg:tt)*) => {
        { #[cfg(debug_assertions)] { eprintln!($($arg)*) } }
    };
}

fn spawn_thread(fun: impl FnOnce() + Send + 'static) {
    let handle = Handle::try_current();
    match handle {
        Ok(handle) => {
            handle.spawn_blocking(fun);
        }
        Err(_) => {
            thread::spawn(fun);
        }
    }
}

#[macro_export]
macro_rules! abort {
    ($($args:tt)*) => {{
        use ::std::borrow::Cow;
        let msg = ::std::format_args!($($args)*)
            .as_str()
            .map_or_else(
                || Cow::Owned(::std::format!($($args)*)), 
                Cow::Borrowed
            );
        
        $crate::err::error(&msg);
        ::std::process::abort()
    }};
}

#[macro_export]
macro_rules! abort_unreachable {
    ($($args:tt)*) => {
        $crate::abort!("UNREACHABLE CONDITION: {}\n\n\n\naborting...", ::std::format_args!($($args)*))
    };
}

#[cfg(windows)]
mod sys {
    use windows::core::{w as wide, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONWARNING, MB_OK,
    };

    fn encode_wide(str: &str) -> Vec<u16> {
        str.encode_utf16().chain([0u16]).collect::<Vec<u16>>()
    }

    /// # Safety:
    ///   `err`: has to be a valid, aligned pointer to a constant null-terminated string of 16-bit Unicode characters.
    unsafe fn err_utf16(err: PCWSTR) {
        unsafe {
            MessageBoxW(
                None,
                err,
                wide!("CloudFlare DDNS Error"),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    /// # Safety:
    ///   `warning`: has to be a valid, aligned pointer to a constant null-terminated string of 16-bit Unicode characters.
    unsafe fn warn_utf16(warning: PCWSTR) {
        unsafe {
            MessageBoxW(
                None,
                warning,
                wide!("CloudFlare DDNS Warning"),
                MB_OK | MB_ICONWARNING,
            );
        }
    }

    pub fn warn(warning: &str) {
        let warning = encode_wide(warning);
        unsafe { warn_utf16(PCWSTR::from_raw(warning.as_ptr())) }
    }

    pub fn err(err: &str) {
        let err = encode_wide(err);
        unsafe { err_utf16(PCWSTR::from_raw(err.as_ptr())) }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::CFOptionFlags;
    use core_foundation_sys::user_notification::{
        kCFUserNotificationCautionAlertLevel, kCFUserNotificationStopAlertLevel,
        CFUserNotificationDisplayAlert,
    };

    fn present_alert(title: &str, message: &str, flags: CFOptionFlags) {
        let header = CFString::new(title);
        let message = CFString::new(message);
        unsafe {
            CFUserNotificationDisplayAlert(
                0.0,
                flags,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                header.as_concrete_TypeRef(),
                message.as_concrete_TypeRef(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
    }

    #[cold]
    #[inline(never)]
    pub fn warn(warning: &str) {
        present_alert(
            "CloudFlare DDNS Warning",
            warning,
            kCFUserNotificationCautionAlertLevel,
        );
    }

    #[cold]
    #[inline(never)]
    pub fn err(err: &str) {
        present_alert(
            "CloudFlare DDNS Error",
            err,
            kCFUserNotificationStopAlertLevel,
        );
    }
}

#[cold]
#[inline(never)]
pub fn error(err: &str) {
    dbg_println!("Error: {err}");
    sys::err(err)
}

#[cold]
#[inline(never)]
pub fn warn(warning: &str) {
    dbg_println!("Warning: {warning}");
    sys::warn(warning)
}

pub async fn spawn_message_box(semaphore: Arc<Semaphore>, err: impl FnOnce() + Send + 'static) {
    if let Ok(permit) = semaphore.acquire_owned().await {
        spawn_thread(move || {
            err();
            drop(permit);
        });
    }
}

fn hook(info: &PanicInfo) {
    macro_rules! try_cast {
        ([$payload:expr] $type: ty $(, $rest: ty)* |> $default: expr) => {
            match $payload.downcast_ref::<$type>() {
                Some(s) => s,
                None => try_cast!([$payload] $($rest),* |> $default),
            }
        };
        ([$_:expr] |> $default: expr) => { $default };
    }

    let msg = try_cast!([info.payload()] String,&str,Box<str>,Rc<str>,Arc<str>,Cow<str> |> "dyn Any + Send + 'static");

    dbg_println!("We panicked at: {msg}");

    match Handle::try_current().as_ref().map(Handle::runtime_flavor) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(|| error(msg)),
        _ => error(msg),
    }
}

pub fn set_hook() {
    std::panic::set_hook(Box::new(hook));
}
