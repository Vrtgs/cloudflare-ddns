use std::fmt::{Display, Formatter, Write};
use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::{Instant, Interval, MissedTickBehavior};

#[inline]
pub fn num_cpus() -> NonZeroUsize {
    static NUM_CPUS: OnceLock<NonZeroUsize> = OnceLock::new();

    #[cold]
    #[inline(never)]
    fn num_cpus_uncached() -> NonZeroUsize {
        std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
    }

    *NUM_CPUS.get_or_init(num_cpus_uncached)
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
