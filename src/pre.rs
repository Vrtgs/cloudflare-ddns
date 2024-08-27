use crate::err;

#[cfg(unix)]
fn ensure_root() {
    use nix::unistd::Uid;
    use std::convert::Infallible;
    use std::io;
    use std::os::unix::process::CommandExt;

    if !Uid::effective().is_root() {
        fn elevate() -> io::Result<Infallible> {
            let err = std::process::Command::new("sudo")
                // contains exe path
                .args(std::env::args_os())
                .exec();
            Err(err)
        }

        elevate().unwrap_or_else(|e| crate::abort!("{e}"));
    }
}

fn set_working_dir() {
    #[cfg(not(feature = "dev-build"))]
    {
        use std::env;

        env::current_exe()
            .and_then(|mut path| {
                path.pop();
                env::set_current_dir(&path)
            })
            .unwrap_or_else(|e| crate::abort!("{e}"));
    }
}

#[cfg(target_os = "macos")]
fn add_to_startup() {
    ensure_root();

    fn inner() -> std::io::Result<()> {
        const LAUNCHD_FILE: &str = "/Library/LaunchDaemons/xyz.vrtgs.cloudflare-ddns.plist";
        std::fs::write(
            LAUNCHD_FILE,
            format!(
                include_str!("../includes/macos_launchd.plist"),
                program_path = std::env::current_exe()?.display()
            ),
        )?;
        std::process::Command::new("launchctl")
            .args(["load", "-w", LAUNCHD_FILE])
            .status()?
            .success()
            .then_some(())
            .ok_or_else(|| std::io::Error::other("failed to load launchd file"))
    }
    inner().unwrap_or_else(|e| crate::abort!("{e}"));
}

#[cfg(target_os = "linux")]
fn add_to_startup() {
    fn inner() -> std::io::Result<()> {
        const SYSTEMD_FILE: &str = "/etc/systemd/system/cloudflare-ddns.service";
        std::fs::write(
            SYSTEMD_FILE,
            format!(
                include_str!("../includes/cloudflare-ddns.service"),
                program_path = std::env::current_exe()?.display()
            ),
        )?;
        std::process::Command::new("systemctl")
            .args(["enable", "--now", "cloudflare-ddns"])
            .status()?
            .success()
            .then_some(())
            .ok_or_else(|| std::io::Error::other("failed to enable systemd service file"))
    }
    inner().unwrap_or_else(|e| crate::abort!("{e}"));

    std::process::exit(0)
}

#[cfg(target_os = "windows")]
fn add_to_startup() {
    todo!("add to startup on windows")
}

#[cfg(target_os = "macos")]
fn remove_from_startup() {
    ensure_root();

    fn inner() -> std::io::Result<()> {
        const LAUNCHD_FILE: &str = "/Library/LaunchDaemons/xyz.vrtgs.cloudflare-ddns.plist";
        std::process::Command::new("launchctl")
            .args(["unload", "-w", "xyz.vrtgs.cloudflare-ddns"])
            .status()?
            .success()
            .then_some(())
            .ok_or_else(|| std::io::Error::other("failed to unload launchd file"))
    }
    inner().unwrap_or_else(|e| crate::abort!("{e}"));
}

#[cfg(target_os = "linux")]
fn remove_from_startup() {
    todo!("remove from startup on linux")
}

#[cfg(target_os = "windows")]
fn remove_from_startup() {
    todo!("remove from startup on windows")
}

fn make_config() {
    fn inner() -> std::io::Result<()> {
        std::fs::create_dir_all("./config")?;

        macro_rules! include {
            ($($name:literal),*) => {$(
            std::fs::write(concat!("./config/", $name, ".toml"), include_str!(concat!("../includes/", $name, ".toml")))?;
            )*};
        }

        include!("api", "http", "misc", "sources");
        Ok(())
    }

    inner().unwrap_or_else(|e| crate::abort!("{e}"));
}

pub fn pre_run() {
    err::set_hook();
    #[cfg(target_os = "linux")]
    ensure_root();

    set_working_dir();

    if 2 < std::env::args().count() {
        panic!("expected at most one argument to be passed!")
    }

    match std::env::args().nth(1).as_deref() {
        Some("add-to-startup") => add_to_startup(),
        Some("remove-from-startup") => remove_from_startup(),
        Some("make-config") => make_config(),
        Some(arg) => panic!("unexpected subcommand: {arg}"),
        None => return,
    }

    std::process::exit(0);
}
