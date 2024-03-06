use std::convert::Infallible;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::panic::UnwindSafe;
use std::thread;
use tokio::runtime::Handle;
use windows::core::{PCWSTR};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONASTERISK, MB_ICONERROR, MB_OK, MessageBoxW};

/// used as the value for std::panic::panic_any, when the panic has already shown an error
struct HandledPanic {
    #[cfg(debug_assertions)] message: Box<str>
}

#[macro_export]
macro_rules! wide_str {
    ($s: expr) => {
        ::windows::core::PCWSTR::from_raw($crate::wide_str!(wide; $s).as_ptr())
    };
    (wide; $s: expr) => {{
        const INPUT: &[u8] = $s.as_bytes();
        const OUTPUT_LEN: usize = ::windows::core::utf16_len(INPUT) + 1;
        static OUTPUT: &[u16; OUTPUT_LEN] = {
            let mut buffer = [0; OUTPUT_LEN];
            let mut input_pos = 0;
            let mut output_pos = 0;
            while let Some((mut code_point, new_pos)) = ::windows::core::decode_utf8_char(INPUT, input_pos) {
                input_pos = new_pos;
                if code_point <= 0xffff {
                    buffer[output_pos] = code_point as u16;
                    output_pos += 1;
                } else {
                    code_point -= 0x10000;
                    buffer[output_pos] = 0xd800 + (code_point >> 10) as u16;
                    output_pos += 1;
                    buffer[output_pos] = 0xdc00 + (code_point & 0x3ff) as u16;
                    output_pos += 1;
                }
            }
            &{ buffer }
        };
        OUTPUT
    }}
}

#[macro_export]
macro_rules! assert {
    ($cond:expr, $($tokens:tt)*) => {
        if $cond {
            Ok(())
        } else {
            Err(err!($($tokens)*))
        }
    };
}

#[macro_export]
macro_rules! err {
    ($err_msg:literal, $code:literal) => {
        Box::new(move ||
            #[allow(unreachable_code)]
            // wide_str returns a valid pointer to a constant null-terminated string of 16-bit Unicode characters
            unsafe { panic_err_utf16(wide_str!(concat!($err_msg, "; Error code: ", $code))) }
                as std::convert::Infallible
        ) as Panic
    };
    (f!$err_msg:literal, $code:literal) => {
        Box::new({
            let msg = format!($err_msg) + concat!("; Error code: ", $code);
            #[allow(unreachable_code)]
            move || panic_err(&msg) as std::convert::Infallible
        }) as Panic
    };
}

#[macro_export]
macro_rules! dbg_println {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) { println!($($arg)*) }
    };
}

pub type Panic = Box<dyn 'static + UnwindSafe + FnOnce() -> Infallible>;
pub type MayPanic<T> = Result<T, Panic>;


/// # Safety:
///   `err_msg`: has to be a valid aligned pointer to a constant null-terminated string of 16-bit Unicode characters.
#[cfg(windows)]
#[cold]
#[inline(never)]
pub unsafe fn err_utf16(err_msg: PCWSTR) {
    unsafe {
        MessageBoxW(
            None,
            err_msg,
            wide_str!("CloudFlare DDNS Error"),
            MB_OK | MB_ICONERROR
        );
    }
}

#[cfg(windows)]
#[cold]
#[inline(never)]
pub fn warn(warning: &str) {
    let warning = warning.encode_utf16()
        .chain(std::iter::once(0u16))
        .collect::<Vec<_>>();

    let fun = move || {
        unsafe {
            MessageBoxW(
                None,
                PCWSTR::from_raw(warning.as_ptr()),
                wide_str!("CloudFlare DDNS Error"),
                MB_OK | MB_ICONASTERISK
            );
        }
    };

    let handle = Handle::try_current();
    match handle {
        Ok(handle) => { handle.spawn_blocking(fun); },
        Err(_) => { thread::spawn(fun); }
    }
}

#[cfg(windows)]
#[cold]
#[inline(never)]
pub fn err(err: &str) {
    let err_msg = err.encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    unsafe { err_utf16(PCWSTR::from_raw(err_msg.as_ptr())) }
}

#[cfg(windows)]
#[cold]
#[inline(never)]
pub fn panic_err(err_msg: &str) -> ! {
    err(err_msg);

    let handled: HandledPanic;
    #[cfg(not(debug_assertions))]
    { handled = HandledPanic; }
    #[cfg(debug_assertions)]
    { handled = HandledPanic { message: Box::from(err_msg)} }

    std::panic::panic_any(handled)
}


/// # Safety:
///   see [`err_utf16`]
#[allow(unused)]
#[cfg(windows)]
#[cold]
#[inline(never)]
pub unsafe fn panic_err_utf16(err_msg: PCWSTR) -> ! {
    err_utf16(err_msg);

    let handled: HandledPanic;
    #[cfg(not(debug_assertions))]
    { handled = HandledPanic; }
    #[cfg(debug_assertions)]
    {
        handled = HandledPanic {
            message: Box::from(&*OsString::from_wide(err_msg.as_wide()).to_string_lossy()),
        }
    }

    std::panic::panic_any(handled)
}

pub fn set_hook() {
    std::panic::set_hook(Box::new(|info| {
        macro_rules! try_cast {
            ($type: ty $(, $rest: ty)* |> $default: expr) => {
                match info.payload().downcast_ref::<$type>() {
                    Some(s) => s,
                    None => try_cast!($($rest),* |> $default),
                }
            };
            (|> $default: expr) => {
                $default
            };
        }

        let msg: &str = match info.payload().downcast_ref::<HandledPanic>() {
            #[cfg(debug_assertions)]
            Some(HandledPanic { message }) => {
                dbg_println!("We panicked responsibly at: {message}");
                return
            },

            #[cfg(not(debug_assertions))]
            Some(HandledPanic{}) => return,
            None => try_cast!(String, &str, Box<str> |> "dyn Any + Send + 'static"),
        };

        dbg_println!("We panicked at: {msg}");

        err(msg)
    }));
}