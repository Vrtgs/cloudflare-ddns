use std::convert::Infallible;
use std::panic::UnwindSafe;
use tokio::task::JoinHandle;
use crate::dbg_println;

/// used as the value for std::panic::resume_unwind, when the panic wants to exit the process
/// cleaning up any stack frames in the process
/// we do a std::panic::resume_unwind to avoid the panic hook
struct ExitPanic {
    status: u8
}

#[cold]
#[inline(never)]
pub fn exit(status: u8) -> ! {
    std::panic::resume_unwind(Box::new(ExitPanic { status }))
}

pub fn catch_exit(panic: impl FnOnce() -> Infallible + UnwindSafe) -> Option<u8> {
    match std::panic::catch_unwind(panic) {
        Ok(never) => match never {  },
        Err(payload) => {
            if let Some(ExitPanic { status }) = payload.downcast_ref::<ExitPanic>() {
                dbg_println!("caught exit");
                return Some(*status);
            }
            dbg_println!("caught panic");
            None
        }
    }
}

macro_rules! any {
    ($($fut: expr),* $(,)?) => {
        tokio::select! {
            $(Some(()) = $fut => return,)*
            else => ::std::future::pending().await
        }
    };
}

pub struct ExitListener {
    handle: Option<JoinHandle<()>>
}

#[cfg(windows)]
mod sys {
    use tokio::signal::windows as signal;
    use crate::err::ExitListener;

    impl ExitListener {
        pub fn new() -> Self {
            let handle = tokio::spawn(async {
                let mut ctrl_c = signal::ctrl_c().unwrap();
                let mut r#break = signal::ctrl_break().unwrap();
                let mut close = signal::ctrl_close().unwrap();
                let mut logoff = signal::ctrl_logoff().unwrap();
                let mut shutdown = signal::ctrl_shutdown().unwrap();
                any!(
                    ctrl_c.recv(),
                    r#break.recv(),
                    close.recv(),
                    logoff.recv(),
                    shutdown.recv()
                )
            });
            
            Self { handle: Some(handle) }
        }
    }
}

#[cfg(unix)]
mod sys {
    use tokio::signal::unix as signal;
    use crate::err::ExitListener;

    impl ExitListener {
        pub fn new() -> Self {
            let handle = tokio::spawn(async {
                let terminate = signal::signal(signal::SignalKind::terminate()).unwrap();
                let quit = signal::signal(signal::SignalKind::quit()).unwrap();
                let hangup = signal::signal(signal::SignalKind::hangup()).unwrap();
                let interrupt = signal::signal(signal::SignalKind::interrupt()).unwrap();
                crate::any!(
                    terminate.recv(),
                    quit.recv(),
                    hangup.recv(),
                    interrupt.recv()
                )
            });

            Self { handle: Some(handle) }
        }
    }
}

impl ExitListener {
    pub async fn recv(&mut self) {
        if let Some(ref mut handle) = self.handle {
            handle.await.unwrap(); // forward any task panics
            drop(self.handle.take())
        }
    }
}