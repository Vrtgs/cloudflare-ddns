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
        Ok(handle) => { handle.spawn_blocking(fun); },
        Err(_) => { thread::spawn(fun); }
    }
}

#[cfg(windows)]
mod sys {
    use windows::core::{PCWSTR, w as wide};
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONASTERISK, MB_ICONERROR, MB_OK, MessageBoxW};

    fn encode_wide(str: &str) -> Vec<u16> {
        str.encode_utf16().chain([0u16]).collect::<Vec<u16>>()
    }

    /// # Safety:
    ///   `err`: has to be a valid, aligned pointer to a constant null-terminated string of 16-bit Unicode characters.
    pub unsafe fn err_utf16(err: PCWSTR) {
        unsafe {
            MessageBoxW(
                None,
                err,
                wide!("CloudFlare DDNS Error"),
                MB_OK | MB_ICONERROR
            );
        }
    }

    /// # Safety:
    ///   `warning`: has to be a valid, aligned pointer to a constant null-terminated string of 16-bit Unicode characters.
    pub unsafe fn warn_utf16(warning: PCWSTR) {
        unsafe {
            MessageBoxW(
                None,
                warning,
                wide!("CloudFlare DDNS Warning"),
                MB_OK | MB_ICONASTERISK
            );
        }
    }

    #[cfg(windows)]
    #[cold]
    #[inline(never)]
    pub fn warn(warning: &str) {
        dbg_println!("Warning: {warning}");
        let warning = encode_wide(warning);
        unsafe { warn_utf16(PCWSTR::from_raw(warning.as_ptr())) }
    }

    #[cfg(windows)]
    #[cold]
    #[inline(never)]
    pub fn err(err: &str) {
        dbg_println!("Error: {err}");
        let err = encode_wide(err);
        unsafe { err_utf16(PCWSTR::from_raw(err.as_ptr())) }
    }
}


pub use sys::{err, warn};

pub async fn spawn_message_box(semaphore: Arc<Semaphore>, err: impl FnOnce() + Send + 'static) {
    if let Ok(permit) = semaphore.acquire_owned().await {
        spawn_thread(move || {
            err();
            drop(permit);
        });
    }
}

fn hook(_info: &PanicInfo)  {
    macro_rules! try_cast {
        ($type: ty $(, $rest: ty)* |> $default: expr) => {
            match _info.payload().downcast_ref::<$type>() {
                Some(s) => s,
                None => try_cast!($($rest),* |> $default),
            }
        };
        (|> $default: expr) => { $default };
    }
    
    let msg = try_cast!(String,&str,Box<str>,Rc<str>,Arc<str>,Cow<str> |> "dyn Any + Send + 'static");

    dbg_println!("We panicked at: {msg}");
    
    match Handle::try_current().as_ref().map(Handle::runtime_flavor) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(|| err(msg)),
        _ => err(msg)
    }
}

pub fn set_hook() {
    std::panic::set_hook(Box::new(hook));
}