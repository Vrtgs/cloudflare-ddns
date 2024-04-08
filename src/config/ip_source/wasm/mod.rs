mod driver;

use crate::util::new_skip_interval_after;
use anyhow::Result;
pub use driver::WasmDriver;
use std::path::Path;
use std::sync::Once;
use std::thread;
use std::time::Duration;
use tokio::sync::{OnceCell, RwLock};

pub static WASM_DRIVER: RwLock<OnceCell<WasmDriver>> = RwLock::const_new(OnceCell::const_new());

#[doc(hidden)]
pub(crate) fn __init_cleanup_routine() {
    static WASM_DRIVER_CLEAN_INIT: Once = Once::new();

    #[cold]
    #[inline(never)]
    fn inner() {
        // the runtime is saved and this **never** dies
        thread::spawn(|| {
            let main = async {
                let mut interval = new_skip_interval_after(Duration::from_secs(3 * 60));
                loop {
                    interval.tick().await;
                    if let Some(driver) = WASM_DRIVER.write().await.take() {
                        let res = driver.close().await;
                        if res.is_err() {
                            tokio::task::spawn_blocking(|| {
                                #[allow(clippy::panicking_unwrap)]
                                // triggers err::hook
                                std::panic::catch_unwind(|| res.unwrap())
                            });
                        }
                    }
                }
            };

            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(main)
        });
    }

    WASM_DRIVER_CLEAN_INIT.call_once(inner);
}

#[doc(hidden)]
pub(crate) async fn __try_get_driver(path: &Path) -> Result<WasmDriver> {
    WasmDriver::open(path).await
}

macro_rules! with_wasm_driver {
    ($token:tt |$driver: ident in ($path: expr)| $($rest:tt)*) => {
        $crate::config::ip_source::wasm::with_wasm_driver!(@assert_async ($token $token) |$driver in ($path)| $($rest)*)
    };

    (@assert_async (async $t:tt) |$driver: ident in ($path: expr)| $lambda: expr) => {$t {
        $crate::config::ip_source::wasm::__init_cleanup_routine();
        let guard = $crate::config::ip_source::wasm::WASM_DRIVER.read().await;
        let $driver = guard
            .get_or_try_init(|| $crate::config::ip_source::wasm::__try_get_driver($path))
            .await?;

        ::anyhow::Ok(async { $lambda }.await?)
    }};
}

pub(crate) use with_wasm_driver;
