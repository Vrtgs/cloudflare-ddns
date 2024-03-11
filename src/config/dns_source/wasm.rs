mod pipe;

use std::path::Path;
use std::thread;
use std::time::{Duration};
use once_cell::sync::Lazy;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio::time::MissedTickBehavior;
use wasi_common::tokio::WasiCtxBuilder;
use wasi_common::WasiCtx;
use wasmtime::{Config, Engine, EngineWeak, Error, Linker, Module, OptLevel, ProfilingStrategy, Store, TypedFunc, WasmBacktraceDetails};
use wasmtime::component::__internal::anyhow;
use crate::config::dns_source::wasm::pipe::ReadWritePipe;

pub struct DdnsStep {
    stdout_pipe: ReadWritePipe,
    stdin_pipe: ReadWritePipe,
    store: Mutex<Store<WasiCtx>>,
    main: TypedFunc<u32, u32>
}

impl DdnsStep {
    pub async fn new(module_path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::_new(module_path.as_ref(), &ENGINE).await
    }

    async fn _new(module_path: &Path, engine: &Engine) -> Result<Self, Error> {
        let module = Module::from_binary(engine, &std::fs::read(module_path)?)?;

        let mut linker = Linker::new(engine);
        wasi_common::tokio::add_to_linker(&mut linker, |cx| cx)?;

        let stdout_pipe = ReadWritePipe::new(512);
        let stdin_pipe = ReadWritePipe::new(512);
        let wasi = WasiCtxBuilder::new()
            .inherit_stderr()
            .stdout(Box::new(stdout_pipe.clone()))
            .stdin(Box::new(stdin_pipe.clone()))
            .build();

        let mut store = Store::new(engine, wasi);
        store.epoch_deadline_async_yield_and_update(1);

        let main = linker
            .instantiate_async(&mut store, &module).await?
            .get_typed_func::<u32, u32>(&mut store, "__ddns_step_main__")?;

        Ok(Self {
            stdout_pipe,
            stdin_pipe,
            store: Mutex::new(store),
            main,
        })
    }

    pub async fn run(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut ctx = self.store.lock().await;
        let len = u32::try_from(data.len())
            .map_err(|_| anyhow::anyhow!("data capacity overflow"))?;

        self.stdin_pipe.write(data);
        let len = self.main.call_async(&mut *ctx, len).await?;
        self.stdout_pipe.read(len as usize)
    }
}

fn configured_engine() -> Engine {
    let mut config = Config::new();

    config.async_support(true);
    config.consume_fuel(false);
    config.epoch_interruption(true);
    config.wasm_backtrace(true);
    config.wasm_backtrace_details(WasmBacktraceDetails::Disable);
    config.profiler(ProfilingStrategy::None);
    config.cranelift_opt_level(OptLevel::Speed);

    let engine = Engine::new(&config).expect("Engine::new");
    let weak_engine = engine.weak();

    let yield_task = async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        while let Some(engine) = EngineWeak::upgrade(&weak_engine) {
            engine.increment_epoch(); drop(engine);
            interval.tick().await;
        }
    };

    match Handle::try_current() {
        Ok(handle) => { handle.spawn(yield_task); },
        Err(_) => {
            eprintln!("initializing the global wasm engine outside a tokio runtime,\
                       spawning a tokio runtime to run the clock interrupts...");
            thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_time().build().unwrap().block_on(yield_task)
            });
        }
    }

    engine
}

static ENGINE: Lazy<Engine> = Lazy::new(configured_engine);