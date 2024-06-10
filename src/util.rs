use std::fmt::{Display, Formatter, Write};
use std::{io, thread};
use std::convert::Infallible;
use std::net::{self, Ipv4Addr};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;
use once_cell::sync::Lazy;
use thiserror::Error;
use tokio::runtime::Handle as TokioHandle;
use tokio::time::{Instant, Interval, MissedTickBehavior};
use crate::abort;


pub static GLOBAL_TOKIO_RUNTIME: Lazy<TokioHandle> = Lazy::new(|| {
    macro_rules! rt_abort {
        () => {
            { |e| { abort!("failed to initialize the global tokio runtime due to {e}") } }
        };
    }
    
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(rt_abort!());

    let handle = runtime.handle().clone();
    thread::Builder::new()
        .spawn(move || runtime.block_on(std::future::pending::<Infallible>()))
        .unwrap_or_else(rt_abort!());
    
    handle
});

#[macro_export]
macro_rules! non_zero {
    ($lit: literal) => {{
        const _: () = {
            if $lit == 0 {
                panic!("non zero literal can't be 0")
            }
        };
        $lit.try_into().unwrap()
    }};
}

#[inline]
pub fn num_cpus() -> NonZeroUsize {
    static NUM_CPUS: OnceLock<NonZeroUsize> = OnceLock::new();

    #[cold]
    #[inline(never)]
    fn num_cpus_uncached() -> NonZeroUsize {
        std::thread::available_parallelism().unwrap_or(non_zero!(1))
    }

    *NUM_CPUS.get_or_init(num_cpus_uncached)
}

pub async fn try_exists(path: impl AsRef<Path>) -> io::Result<bool> {
    async fn inner(path: PathBuf) -> io::Result<bool> {
        tokio::task::spawn_blocking(move || path.try_exists())
            .await
            .map_err(|e| io::Error::other(format!("background task failed: {e}")))?
    }

    inner(path.as_ref().to_owned()).await
}

pub fn new_skip_interval(period: Duration) -> Interval {
    new_skip_interval_at(Instant::now(), period)
}

pub fn new_skip_interval_after(period: Duration) -> Interval {
    new_skip_interval_at(Instant::now() + period, period)
}

pub fn new_skip_interval_at(start: Instant, period: Duration) -> Interval {
    let mut interval = tokio::time::interval_at(start, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}

pub struct EscapeJson<'a>(&'a str);

impl<'a> Display for EscapeJson<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut utf16_buf = [0u16; 2];
        for c in self.0.chars() {
            match c {
                '\x08' => f.write_str("\\b"),
                '\x0c' => f.write_str("\\f"),
                '\n' => f.write_str("\\n"),
                '\r' => f.write_str("\\r"),
                '\t' => f.write_str("\\t"),
                '"' => f.write_str("\\\""),
                '\\' => f.write_str("\\"),
                ' ' => f.write_char(' '),
                c if c.is_ascii_graphic() => f.write_char(c),
                c => {
                    let encoded = c.encode_utf16(&mut utf16_buf);
                    for utf16 in encoded {
                        write!(f, "\\u{:04X}", utf16)?;
                    }
                    Ok(())
                }
            }?
        }

        Ok(())
    }
}

pub trait EscapeExt {
    fn escape_json(&self) -> EscapeJson<'_>;
}

impl EscapeExt for str {
    fn escape_json(&self) -> EscapeJson<'_> {
        EscapeJson(self)
    }
}

#[derive(Debug, Error)]
pub enum AddrParseError {
    #[error("The input data was too long to even be considered an address")]
    TooLong,
    #[error("invalid encoding on the addresses bytes")]
    InvalidEncoding,
    #[error(transparent)]
    Parse(#[from] net::AddrParseError),
}

pub trait AddrParseExt: Sized {
    fn parse_ascii_bytes(b: &[u8]) -> Result<Self, AddrParseError>;
}

impl AddrParseExt for Ipv4Addr {
    fn parse_ascii_bytes(b: &[u8]) -> Result<Self, AddrParseError> {
        if b.len() > b"xxx.xxx.xxx.xxx".len() {
            return Err(AddrParseError::TooLong);
        }

        b.is_ascii()
            .then(|| unsafe { std::str::from_utf8_unchecked(b) })
            .ok_or(AddrParseError::InvalidEncoding)
            .and_then(|s| Ipv4Addr::from_str(s).map_err(Into::into))
    }
}
