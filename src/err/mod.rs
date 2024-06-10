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
    use std::ffi::OsStr;
    use std::num::NonZeroU16;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{w as wide, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONWARNING, MB_OK, MESSAGEBOX_STYLE,
    };

    fn encode_wide(str: &OsStr) -> Vec<u16> {
        str.encode_wide()
            .filter_map(NonZeroU16::new)
            .map(NonZeroU16::get)
            .chain([0u16])
            .collect::<Vec<u16>>()
    }

    /// # Safety:
    ///   `caption`: has to be a valid, aligned pointer to a constant null-terminated string of 16-bit Unicode characters.
    unsafe fn present_alert(caption: PCWSTR, msg: &OsStr, style: MESSAGEBOX_STYLE) {
        let msg = encode_wide(msg);
        unsafe {
            MessageBoxW(None, PCWSTR::from_raw(msg.as_ptr()), caption, MB_OK | style);
        }
    }

    pub fn warn(warning: &str) {
        // # Safety: caption was made by the wide macro which is valid
        unsafe {
            present_alert(
                wide!("CloudFlare DDNS Warning"),
                warning.as_ref(),
                MB_ICONWARNING,
            )
        }
    }

    pub fn err(err: &str) {
        // # Safety: caption was made by the wide macro which is valid
        unsafe { present_alert(wide!("CloudFlare DDNS Warning"), err.as_ref(), MB_ICONERROR) }
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

    pub fn warn(warning: &str) {
        present_alert(
            "CloudFlare DDNS Warning",
            warning,
            kCFUserNotificationCautionAlertLevel,
        );
    }

    pub fn err(err: &str) {
        present_alert(
            "CloudFlare DDNS Error",
            err,
            kCFUserNotificationStopAlertLevel,
        );
    }
}

#[cfg(target_os = "linux")]
mod sys {
    pub fn warn(_: &str) {}

    pub fn err(_: &str) {}
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
