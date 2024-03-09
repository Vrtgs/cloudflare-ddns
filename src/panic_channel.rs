use std::collections::HashSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use tokio::sync::Notify;


pub enum UpdaterEvent {
    Update,
    ServiceExited(UpdaterExitStatus)
}

pub enum UpdaterExitStatus {
    Panic { name: & 'static str },
    Error { name: & 'static str, err: Box<dyn Error + Send> }
}

impl Display for UpdaterExitStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match *self {
            UpdaterExitStatus::Panic { name } =>
                write!(f, "updater <{name}> died unexpectedly"),
            
            UpdaterExitStatus::Error { name, ref err } =>
                write!(f, "updater <{name}> exited with the error: {err}")
        }
    }
}

pub struct UpdatersManager {
    rcv: UnboundedReceiver<UpdaterExitStatus>,
    snd: UnboundedSender<UpdaterExitStatus>,
    notifier: Arc<Notify>,
    active_services: HashSet<&'static str>,
}

impl UpdatersManager {
    
    /// watches for service changes
    pub async fn watch(&mut self) -> UpdaterEvent {
        tokio::select! {
            _ = self.notifier.notified() => UpdaterEvent::Update,
            state = self.rcv.recv() => {
                use UpdaterExitStatus as E;
                
                let state = state
                    .expect("we always hold at least one sender, and we never close");
                
                let (E::Error{ name, .. }|E::Panic { name, .. }) = state;
                assert!(self.active_services.remove(name), "updater returned an invalid name");
                
                UpdaterEvent::ServiceExited(state)
            }
        }
    }

    #[inline(always)]
    pub fn add_updater(&mut self, name: &'static str) -> Updater {
        assert!(self.active_services.insert(name), "services must have a unique name");
        Updater {
            name,
            notifier: self.notifier.clone(),
            snd: Some(self.snd.clone())
        }
    }
}

#[derive(Clone)]
pub struct Updater {
    name: &'static str,
    notifier: Arc<Notify>,
    snd: Option<UnboundedSender<UpdaterExitStatus>>
}

impl Updater {
    #[inline(always)]
    pub fn update(&self) { self.notifier.notify_waiters() }
    
    pub fn shutdown(self, err: impl Error + Send + 'static) {
        self.shutdown_box(Box::new(err))
    }

    pub fn shutdown_box(mut self, err: Box<dyn Error + Send>) {
        let snd = self.snd.take().unwrap();
        let _ = snd.send(UpdaterExitStatus::Error { name: self.name, err });
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        if let Some(snd) = self.snd.take() {
            let _ = snd.send(UpdaterExitStatus::Panic { name: self.name });
        }
    }
}

impl UpdatersManager {
    #[inline(always)]
    pub fn new() -> Self {
        let (snd, rcv) =
            tokio::sync::mpsc::unbounded_channel();
        UpdatersManager { rcv, snd, notifier: Arc::new(Notify::new()), active_services: HashSet::new() }
    }
}
