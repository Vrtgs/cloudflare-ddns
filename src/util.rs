use std::fmt::{Display, Formatter, Write};
use std::net::{self, Ipv4Addr};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;
use std::{io, thread};
use thiserror::Error;
use tokio::time::{Instant, Interval, MissedTickBehavior};

#[macro_export]
macro_rules! non_zero {
    ($x: expr) => {{
        const {
            match ::std::num::NonZero::new($x) {
                Some(x) => x,
                None => panic!("non zero can't be 0")
            }
        }
    }};
}

#[inline]
pub fn num_cpus() -> NonZero<usize> {
    static NUM_CPUS: OnceLock<NonZero<usize>> = OnceLock::new();

    #[cold]
    #[inline(never)]
    fn num_cpus_uncached() -> NonZero<usize> {
        thread::available_parallelism().unwrap_or(non_zero!(1))
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
