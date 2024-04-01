use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::time::{Instant, Interval, MissedTickBehavior};

#[inline]
pub fn num_cpus() -> NonZeroUsize {
    static NUM_CPUS: OnceLock<NonZeroUsize> = OnceLock::new();

    #[cold]
    fn num_cpus_uncached() -> NonZeroUsize {
        std::thread::available_parallelism()
            .unwrap_or(NonZeroUsize::MIN)
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