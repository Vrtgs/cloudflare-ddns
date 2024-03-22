mod driver;

use std::sync::Once;
use std::time::{Duration, Instant};
use anyhow::Result;
use tokio::sync::{OnceCell, RwLock};
pub use driver::WasmDriver;

pub static WASM_DRIVER: RwLock<OnceCell<WasmDriver>> = RwLock::const_new(OnceCell::const_new());

const DRIVER_PATH: &str = "./ddns-wasm-runtime.exe";


#[doc(hidden)]
pub(crate) fn __init_cleanup_routine() {
    static WASM_DRIVER_CLEAN_INIT: Once = Once::new();

    #[cold]
    #[inline(never)]
    fn inner() {
        tokio::spawn(async {
            let mut interval = tokio::time::interval_at(
                (Instant::now() + Duration::from_secs(3 * 60)).into(),
                Duration::from_secs(3 * 60)
            );

            loop {
                interval.tick().await;
                if let Some(driver) = WASM_DRIVER.write().await.take() {
                    let res = driver.close().await;
                    let _ = std::panic::catch_unwind(|| res.unwrap());
                }
            }
        });
    }

    WASM_DRIVER_CLEAN_INIT.call_once(inner);
}

#[inline]
#[doc(hidden)]
pub(crate) async fn __try_get_driver() -> Result<WasmDriver> {
    WasmDriver::open(DRIVER_PATH).await
}

macro_rules! with_wasm_driver {
    ($t:tt |$driver: ident $(: $ty: ty)?| $($rest:tt)*) => {
        $crate::config::ip_source::wasm::with_wasm_driver!(@assert_async ($t $t) |$driver $(: $ty)?| $($rest)*)
    };

    (@assert_async (async $t:tt) |$driver: ident $(: $ty: ty)?| $lambda: expr) => {$t {
        $crate::config::ip_source::wasm::__init_cleanup_routine();
        let guard = $crate::config::ip_source::wasm::WASM_DRIVER.read().await;
        let $driver $(: $ty)? = guard
            .get_or_try_init($crate::config::ip_source::wasm::__try_get_driver)
            .await?;

        ::anyhow::Ok(async { $lambda }.await?)
    }};
}

pub(crate) use with_wasm_driver;