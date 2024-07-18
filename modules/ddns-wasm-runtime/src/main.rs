mod pipe;

use std::borrow::Cow;
use std::collections::BTreeMap;
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader, ErrorKind};
use std::io::{BufRead, stdout, Write};
use std::mem::size_of;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;
use anyhow::anyhow;
use bincode::{Decode, enc, Encode};
use anyhow::Result;
use bincode::config::{Configuration, Fixint, LittleEndian, NoLimit};
use bincode::enc::EncoderImpl;
use bincode::enc::write::SizeWriter;
use bincode::error::EncodeError;
use interprocess::local_socket::{ListenerOptions, Name};
use interprocess::local_socket::tokio::{SendHalf, RecvHalf};
use interprocess::local_socket::traits::tokio::{Listener, Stream};
use once_cell::sync::Lazy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Handle as TokioHandle;
use tokio::sync::{Mutex, OnceCell};
use tokio::task::JoinSet;
use tokio::time::{Instant, Interval, MissedTickBehavior, timeout};
use wasi_common::tokio::WasiCtxBuilder;
use wasi_common::WasiCtx;
use wasmtime::{Config as WasmConfig, Engine, EngineWeak, InstanceAllocationStrategy, Linker, Module, MpkEnabled, OptLevel, PoolingAllocationConfig, ProfilingStrategy, Store, TypedFunc, UpdateDeadline, WasmBacktraceDetails};
use pipe::{ReadWritePipe, SharedCtxFile};


const EPOCH_DURATION: Duration = Duration::from_millis(25);

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct SharedCtx {
    stdout_pipe: ReadWritePipe,
    stdin_pipe: ReadWritePipe,
    store: Mutex<Store<WasiCtx>>
}

impl SharedCtx {
    pub async fn gc(&self) {
        // again use store as the lock
        let mut store_lock = self.store.lock().await;
        store_lock.gc();
        self.stdin_pipe.gc();
        self.stdout_pipe.gc();
    }
}

#[derive(Clone)]
pub struct WasmDdnsStep {
    ctx: Arc<SharedCtx>,
    main: TypedFunc<u32, i32>
}

fn interval_after(period: Duration) -> Interval {
    tokio::time::interval_at(Instant::now() + period, period)
}

impl WasmDdnsStep {
    pub async fn new(module_path: impl AsRef<str>) -> Result<Self> {
        let (engine, linker) = &*ENGINE;
        Self::_new(module_path.as_ref(), engine, linker).await
    }

    async fn _new(module_path: &str, engine: &Engine, linker: &Linker<WasiCtx>) -> Result<Self> {
        let (module, ctx) = tokio::task::spawn_blocking({
            let binary = tokio::fs::read(module_path).await?;
            let engine = engine.clone();
            
            move || {

                let pre_compiled = engine.precompile_module(&binary)?; drop(binary);
                // Safety: deserializes a compiled module  created with Engine::precompile_module
                let module = unsafe { Module::deserialize(&engine, pre_compiled)? };
                let (file, ctx_snd) = SharedCtxFile::new();
                
                let wasi = WasiCtxBuilder::new()
                    .inherit_stderr()
                    .stdout(Box::new(file.clone()))
                    .stdin(Box::new(file))
                    .build();

                let mut store = Store::new(&engine, wasi);

                store.epoch_deadline_callback(|_| Ok(UpdateDeadline::Yield(1)));

                let ctx = Arc::new(SharedCtx {
                    stdout_pipe: ReadWritePipe::new(),
                    stdin_pipe: ReadWritePipe::new(),
                    store: Mutex::new(store)
                });

                let weak_ctx = Arc::downgrade(&ctx);
                let gc_ctx = Weak::clone(&weak_ctx);

                ctx_snd.send(weak_ctx).expect("file receiver dropped");

                tokio::spawn(async move {
                    let mut interval = interval_after(Duration::from_secs(120));

                    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    interval.tick().await;

                    while let Some(ctx) = Weak::upgrade(&gc_ctx) {
                        ctx.gc().await; drop(ctx);
                        interval.tick().await;
                    }
                });

                anyhow::Ok((module, ctx))
            }
        }).await??;
        
        
        let mut store_lock = ctx.store.lock().await;

        let main = linker
            .instantiate_async(&mut *store_lock, &module).await?
            .get_typed_func::<u32, i32>(&mut *store_lock, "__ddns_step_main__")?;
        
        drop(store_lock);

        Ok(Self {
            ctx,
            main,
        })
    }

    pub async fn run(&self, data: &[u8]) -> Result<Vec<u8>> {
        let len = u32::try_from(data.len())
            .map_err(|_| anyhow::anyhow!("data capacity overflow"))?;

        let ctx = &*self.ctx;

        // although omitted to avoid deadlocks with the files
        // the entire context should be protected by a Mutex
        // and since the store mutex is also the same mutex we use to call the function
        // we first lock it to ensure only one function is called at a time
        let mut store = ctx.store.lock().await;

        ctx.stdin_pipe.write(data);
        let res = self.main.call_async(&mut *store, len).await?;
        let output = ctx.stdout_pipe.take_output();
        match res {
            0 => Ok(output),
            _ => {
                let err = match String::from_utf8_lossy(&output) {
                    // Safety: output is valid utf-8
                    Cow::Borrowed(_valid) => unsafe { String::from_utf8_unchecked(output) },
                    Cow::Owned(x) => x
                };
                Err(anyhow::Error::msg(err))
            }
        }
    }
}

fn configured_engine() -> (Engine, Linker<WasiCtx>) {
    let mut config = WasmConfig::new();

    config.async_support(true);
    config.consume_fuel(false);
    config.epoch_interruption(true);
    config.wasm_backtrace(true);
    config.allocation_strategy({
        let mut cfg = PoolingAllocationConfig::default();
    
        cfg.async_stack_zeroing(false);
        cfg.memory_protection_keys(MpkEnabled::Disable);
        cfg.max_memory_protection_keys(0);
        cfg.max_unused_warm_slots(256);
    
        InstanceAllocationStrategy::Pooling(cfg)
    });
    config.wasm_backtrace_details(WasmBacktraceDetails::Disable);
    config.parallel_compilation(true);
    config.profiler(ProfilingStrategy::None);
    config.cranelift_opt_level(OptLevel::Speed);

    let engine = Engine::new(&config).expect("Engine::new");
    let weak_engine = engine.weak();

    let yield_task = async move {
        let mut interval = interval_after(EPOCH_DURATION);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        while let Some(engine) = EngineWeak::upgrade(&weak_engine) {
            engine.increment_epoch(); drop(engine);
            interval.tick().await;
        }
    };

    match TokioHandle::try_current() {
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
    let mut linker = Linker::new(&engine);
    wasi_common::tokio::add_to_linker(&mut linker, |cx| cx)
        .expect("unable to add tokio imports to linker");

    (engine, linker)
}

static ENGINE: Lazy<(Engine, Linker<WasiCtx>)> = Lazy::new(configured_engine);

#[derive(Decode)]
enum WasmCommand {
    Shutdown,
    Request(Request)
}

#[derive(Decode)]
struct Request {
    id: u64,
    module: Arc<str>,
    data: Box<[u8]>
}

#[derive(Encode)]
struct Response {
    id: u64,
    response: Result<Vec<u8>, String>
}

const BIN_CODE_CONFIG: Configuration<LittleEndian, Fixint, NoLimit> = bincode::config::standard()
    .with_fixed_int_encoding();

async fn ipc_channel() -> Result<(RecvHalf, SendHalf)> {
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            use interprocess::local_socket::GenericFilePath;
            
            let path = tempfile::NamedTempFile::new()?.into_temp_path().keep()?;
            let name_bytes: &str = path.to_str().ok_or_else(|| anyhow!("path contained invalid utf-8"))?;
            let name: Name = interprocess::local_socket::ToFsName::to_fs_name::<GenericFilePath>(
                path
            )?;
        } else {
            use interprocess::local_socket::GenericNamespaced;

            let name_bytes: &str = &format!(r"\\.\pipe\{}", uuid::Uuid::new_v4());
            let name: Name = interprocess::local_socket::ToNsName::to_ns_name::<GenericNamespaced>(name_bytes)?;
        }
    }
    
    let name_bytes = name_bytes.as_bytes();
    
    
    let msg = {
        let mut msg = Vec::with_capacity(
            // max len of u128 + null byte + name_bytes.len()
            40 + 1 + name_bytes.len()
        );
        msg.extend_from_slice(itoa::Buffer::new().format(name_bytes.len()).as_bytes());
        msg.push(b'\0');
        msg.extend_from_slice(name_bytes);
        msg
    };

    let listener_opts = ListenerOptions::new().name(name);
    
    let listener = listener_opts.create_tokio()?;
    
    // make sure we listen before informing where we listen to
    let mut stdout = stdout().lock();
    stdout.write_all(&msg)?;
    stdout.flush()?;
    
    let (read, write) = listener.accept().await?.split();
    
    anyhow::Ok((read, write))
}

async fn write_command<W: AsyncWrite + Unpin>(stream: &mut W, cmd: Response) -> Result<()> {
    struct VecWriter(Vec<u8>);
    impl enc::write::Writer for VecWriter {
        #[inline(always)]
        fn write(&mut self, bytes: &[u8]) -> std::result::Result<(), EncodeError> {
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }

    let encoded = {
        let size = {
            let mut size_writer = EncoderImpl::new(SizeWriter::default(), BIN_CODE_CONFIG);
            cmd.encode(&mut size_writer)?;
            size_writer.into_writer().bytes_written
        };
        let mut writer = VecWriter(Vec::with_capacity(size + size_of::<u64>()));
        writer.0.extend_from_slice(&u64::to_le_bytes(u64::try_from(size)?));
        let mut encoder = EncoderImpl::new(writer, BIN_CODE_CONFIG);
        cmd.encode(&mut encoder)?;
        encoder.into_writer().0
    };

    stream.write_all(&encoded).await?;
    Ok(())
}

async fn get_request<R: AsyncBufRead + Unpin>(read: &mut R) -> Result<Option<Request>> {
    match read.read_u64_le().await {
        Ok(sz) => {
            let sz = usize::try_from(sz)
                .map_err(|_| anyhow!("request data given is too large"))?;
            let mut buf = vec![0; sz];
            read.read_exact(&mut buf).await?;

            let (req, _) = bincode::decode_from_slice::<WasmCommand, _>(&buf, BIN_CODE_CONFIG)?;

            Ok(match req {
                WasmCommand::Request(req) => Some(req),
                WasmCommand::Shutdown => None
            })
        }
        Err(ref e) if e.kind() == ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e.into()),
    }
}

type ModuleCache = BTreeMap<Arc<str>, Arc<OnceCell<WasmDdnsStep>>>;

async fn get_or_init_module(modules: &parking_lot::Mutex<ModuleCache>, key: Arc<str>) -> Result<WasmDdnsStep> {
    let cell = {
        let mut modules_guard = modules.lock();
        let cell = modules_guard
            .entry(Arc::clone(&key))
            .or_insert_with(||Arc::new(OnceCell::new()));

        Arc::clone(cell)
    };

    cell.get_or_try_init(|| WasmDdnsStep::new(key)).await.cloned()
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut joins = JoinSet::new();
    
    let (read, mut write) = timeout(
        Duration::from_secs(15),
        ipc_channel()
    ).await??;


    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<Response>(
            thread::available_parallelism()
                .unwrap_or(NonZeroUsize::MIN).get() * 8
        );

    joins.spawn(async move {
        while let Some(response) = rx.recv().await {
            write_command(&mut write, response).await?;
        }
        anyhow::Ok(Some(write))
    });

    joins.spawn(async move {
        let mut read = BufReader::new(read);
        let mut joins = JoinSet::new();
        let modules = Arc::new(parking_lot::Mutex::new(ModuleCache::new()));
        while let Some(Request { id, module: module_path, data }) = get_request(&mut read).await? {
            if !Path::new(&*module_path).is_absolute() {
                tx.send(Response {
                    id,
                    response: Err("You must provide absolute paths as the module path".to_owned())
                }).await?;
                continue
            }
            
            let tx = tx.clone();
            let modules = Arc::clone(&modules);
            joins.spawn(async move {
                let module = match get_or_init_module(&modules, module_path).await {
                    Ok(module) => module,
                    Err(e) => {
                        tx.send(Response { id, response: Err(e.to_string()) }).await?;
                        return anyhow::Ok(())
                    }
                };
                
                let response = module.run(&data).await
                    .map_err(|e| e.to_string());
                
                tx.send(Response { id, response }).await?;
                anyhow::Ok(())
            });
        }

        while let Some(outgoing_request) = joins.join_next().await {
            outgoing_request??
        }

        anyhow::Ok(None)
    });

    // wait for process to be given the go ahead to perform a cleanup and exit
    joins.spawn_blocking(|| {
        let mut stdin = std::io::stdin().lock();
        loop {
            if let Err(..) | Ok(0) = stdin.read_line(&mut String::new()) {
                break anyhow::Ok(None);
            }
        }
    });
    
    while let Some(next) = joins.join_next().await {
        if let Some(mut sender) = next?? {
            sender.shutdown().await?
        }
    }
    
    Ok(())
}