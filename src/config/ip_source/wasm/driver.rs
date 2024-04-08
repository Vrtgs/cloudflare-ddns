use crate::config::ip_source::GetIpError;
use ahash::RandomState as AHashState;
use anyhow::{anyhow, Context, Result};
use bincode::config::{Configuration, Fixint, LittleEndian, NoLimit};
use bincode::enc::write::SizeWriter;
use bincode::enc::EncoderImpl;
use bincode::error::EncodeError;
use bincode::{enc, Decode, Encode};
use dashmap::DashMap;
use interprocess::local_socket::tokio::{RecvHalf, SendHalf, Stream as LocalSocketStream};
use interprocess::local_socket::traits::tokio::Stream;
use std::io::ErrorKind::UnexpectedEof;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::error::Elapsed;
use tokio::time::{sleep, timeout};

#[derive(Encode)]
enum WasmCommand<'a> {
    Shutdown,
    Request(Request<'a>),
}

#[derive(Encode)]
struct Request<'a> {
    id: u64,
    module: &'a str,
    data: &'a [u8],
}

#[derive(Decode, Debug)]
struct Response {
    id: u64,
    response: Result<Vec<u8>, String>,
}

const BIN_CODE_CONFIG: Configuration<LittleEndian, Fixint, NoLimit> =
    bincode::config::standard().with_fixed_int_encoding();

async fn ipc_channel(child: &mut Child) -> Result<(RecvHalf, SendHalf)> {
    let path = {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("could not create child stdout"))
            .map(BufReader::new)?;

        let mut temp_buf = Vec::with_capacity(256);

        stdout.read_until(b'\0', &mut temp_buf).await?;

        let Some(b'\0') = temp_buf.pop() else {
            anyhow::bail!("could not read path size from child")
        };

        let sz =
            atoi::atoi::<u64>(&temp_buf).with_context(|| "invalid length provided by child")?;

        let sz = usize::try_from(sz)?;
        temp_buf.clear();
        temp_buf.reserve(sz);
        stdout.take(sz as u64).read_to_end(&mut temp_buf).await?;

        anyhow::ensure!(temp_buf.len() == sz, "child provided incorrect length");

        let path = String::from_utf8(temp_buf)?;

        #[cfg(unix)]
        {
            interprocess::local_socket::ToFsName::to_fs_name(path)?
        }
        #[cfg(windows)]
        {
            interprocess::local_socket::ToNsName::to_ns_name(path)?
        }
    };

    anyhow::Ok(LocalSocketStream::connect(path).await?.split())
}

type RequestsMap = DashMap<u64, oneshot::Sender<Result<Vec<u8>, String>>, AHashState>;

type RunArguments = (Box<str>, Vec<u8>, oneshot::Sender<Result<Vec<u8>, String>>);

struct WasmDriverInner {
    write_task: JoinHandle<Result<()>>,
    read_task: JoinHandle<Result<()>>,
    sender: mpsc::Sender<RunArguments>,
}

pub struct WasmDriver(Option<WasmDriverInner>);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

impl WasmDriver {
    async fn read_response<R: AsyncBufRead + Unpin>(stream: &mut R) -> Result<Option<Response>> {
        let resp_len = match stream.read_u64_le().await {
            Ok(len) => len,
            Err(ref e) if e.kind() == UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let resp_len = usize::try_from(resp_len)?;
        let mut resp_buffer = Vec::with_capacity(resp_len);
        stream
            .take(resp_len as u64)
            .read_to_end(&mut resp_buffer)
            .await?;
        anyhow::ensure!(
            resp_buffer.len() == resp_len,
            "child provided invalid response length"
        );

        let (resp, _) = bincode::decode_from_slice::<Response, _>(&resp_buffer, BIN_CODE_CONFIG)?;
        Ok(Some(resp))
    }

    async fn write_command<W: AsyncWrite + Unpin>(
        stream: &mut W,
        cmd: WasmCommand<'_>,
    ) -> Result<()> {
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
            let mut writer = VecWriter(Vec::with_capacity(size + std::mem::size_of::<u64>()));
            writer
                .0
                .extend_from_slice(&u64::to_le_bytes(u64::try_from(size)?));
            let mut encoder = EncoderImpl::new(writer, BIN_CODE_CONFIG);
            cmd.encode(&mut encoder)?;
            encoder.into_writer().0
        };

        stream.write_all(&encoded).await?;
        Ok(())
    }

    pub async fn open(wasm_runtime: impl AsRef<Path>) -> Result<Self> {
        Self::_open(wasm_runtime.as_ref()).await
    }

    async fn _open(path: &Path) -> Result<Self> {
        let owned = path.to_path_buf();
        if let Ok(false) = tokio::task::spawn_blocking(move || owned.try_exists()).await? {
            anyhow::bail!(GetIpError::NoWasmDriver)
        }

        let mut child = Command::new(path)
            .stderr(Stdio::inherit())
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .spawn()?;

        let (recv, mut send) = timeout(Duration::from_secs(15), ipc_channel(&mut child))
            .await
            .with_context(|| "could not connect to child.. timed out")??;

        let (sender, mut receiver) =
            mpsc::channel::<(Box<str>, Vec<u8>, oneshot::Sender<Result<Vec<u8>, String>>)>(256);

        let outgoing_request = Arc::new(RequestsMap::default());
        let requests_map = Arc::clone(&outgoing_request);
        let write_task = tokio::spawn(async move {
            struct EntryManager {
                current_id: u64,
            }

            struct Entry {
                pub id: u64,
                map: Weak<RequestsMap>,
            }

            impl Drop for Entry {
                fn drop(&mut self) {
                    if let Some(map) = Weak::upgrade(&self.map) {
                        map.remove(&self.id);
                    }
                }
            }

            impl EntryManager {
                #[inline(always)]
                #[must_use]
                fn insert(
                    &mut self,
                    map: &Arc<RequestsMap>,
                    sender: oneshot::Sender<Result<Vec<u8>, String>>,
                ) -> Entry {
                    let id = {
                        let tmp = self.current_id;
                        self.current_id = self.current_id.wrapping_add(1);
                        tmp
                    };
                    map.insert(id, sender);
                    Entry {
                        id,
                        map: Arc::downgrade(map),
                    }
                }
            }

            let mut entry_manager = EntryManager { current_id: 0 };
            while let Some((module, data, recv)) = receiver.recv().await {
                let entry_guard = entry_manager.insert(&requests_map, recv);
                Self::write_command(
                    &mut send,
                    WasmCommand::Request(Request {
                        id: entry_guard.id,
                        module: &module,
                        data: &data,
                    }),
                )
                .await?;
                tokio::spawn(async {
                    sleep(REQUEST_TIMEOUT).await;
                    drop(entry_guard)
                });
            }

            tokio::select! {
                _ = async {
                    loop {
                        if requests_map.is_empty() {
                            break
                        }
                        sleep(Duration::from_millis(8)).await
                    }
                } => {}
                _ = sleep(REQUEST_TIMEOUT) => {}
            }

            Self::write_command(&mut send, WasmCommand::Shutdown).await?;
            send.shutdown().await?;
            drop(send);

            if let Err(Elapsed { .. }) = timeout(Duration::from_secs(15), child.wait()).await {
                child.kill().await?;
            }

            anyhow::Ok(())
        });
        let requests_map = outgoing_request;
        let read_task = tokio::spawn(async move {
            let mut recv = BufReader::new(recv);
            while let Some(response) = Self::read_response(&mut recv).await? {
                if let Some((_, sender)) = requests_map.remove(&response.id) {
                    let _ = sender.send(response.response);
                }
            }
            anyhow::Ok(())
        });

        Ok(Self(Some(WasmDriverInner {
            write_task,
            read_task,
            sender,
        })))
    }

    pub async fn run(&self, module: impl AsRef<Path>, data: impl Into<Vec<u8>>) -> Result<Vec<u8>> {
        self._run(module.as_ref(), data.into()).await
    }

    async fn _run(&self, module: &Path, data: Vec<u8>) -> Result<Vec<u8>> {
        let module = module.to_string_lossy().into_owned().into_boxed_str();

        let (tx, rx) = oneshot::channel();
        let inner = self.0.as_ref().unwrap();
        inner.sender.send((module, data, tx)).await?;

        rx.await
            .with_context(|| "request timeout")?
            .map_err(|e| anyhow::anyhow!("failed to run wasm command: {e}"))
    }

    pub async fn close(mut self) -> Result<()> {
        let WasmDriverInner {
            write_task,
            mut read_task,
            sender,
        } = self.0.take().unwrap();

        drop(sender);
        write_task.await??;
        match timeout(REQUEST_TIMEOUT, &mut read_task).await {
            Ok(res) => res??,
            Err(Elapsed { .. }) => read_task.abort(),
        }

        anyhow::Ok(())
    }
}

impl Drop for WasmDriver {
    fn drop(&mut self) {
        if let Some(inner) = self.0.take() {
            drop(inner.sender);
            inner.write_task.abort();
            inner.read_task.abort();
        }
    }
}
