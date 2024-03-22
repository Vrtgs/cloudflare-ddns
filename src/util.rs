use std::num::NonZeroUsize;
use std::sync::OnceLock;

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