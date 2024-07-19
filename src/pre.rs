use std::convert::Infallible;
use std::io;
use crate::{abort, err};

#[cfg(target_os = "linux")]
fn ensure_root() {
    use std::os::unix::process::CommandExt;
    use nix::unistd::Uid;
    
    if !Uid::effective().is_root() {
        fn elevate() -> io::Result<Infallible> {
            let err = std::process::Command::new("sudo")
                .arg(std::env::current_exe()?)
                .args(std::env::args_os())
                .exec();
            Err(err)
        }
        
        elevate().unwrap_or_else(|e| abort!("{e}"));
    }
}

pub fn pre_run() {
    err::set_hook();
    #[cfg(target_os = "linux")]
    ensure_root();
    
}