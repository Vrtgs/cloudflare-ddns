use std::fmt::{Debug, Display, Formatter, Write};
use std::process::Stdio;
use std::{env, io};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::process::Command;
use tokio::try_join;

macro_rules! plaintext_sources {
    () => {
        include!("./default/plaintext_sources")
    };
}

macro_rules! json_sources {
    () => {
        include!("./default/json_sources")
    };
}

async fn make_default_sources_toml() -> io::Result<()> {
    let mut data = String::new();

    let plain_sources = plaintext_sources!();
    for source in plain_sources {
        writeln!(data, r#"["{source}"]"#).unwrap();
        writeln!(data, "steps = [\"Plaintext\"]\n").unwrap();
    }

    let plain_sources = json_sources!();
    for (source, key) in plain_sources {
        writeln!(data, r#"["{source}"]"#).unwrap();
        writeln!(data, r#"steps = [{{ Json = {{ key = "{key}" }} }}]"#).unwrap();
    }

    tokio::fs::write("./default/gen/sources.toml", data.trim()).await
}

async fn make_default_sources_rs() -> io::Result<()> {
    let mut file = BufWriter::new(File::create("./default/gen/sources.array").await?);

    #[derive(Clone)]
    struct VecDebug<T>(Vec<T>);

    impl<T: Debug> Debug for VecDebug<T> {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.write_str("vec!")?;
            <[T] as Debug>::fmt(&self.0, f)
        }
    }

    #[derive(Clone)]
    struct DisplayStr(String);

    impl Debug for DisplayStr {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            <str as Display>::fmt(&self.0, f)
        }
    }

    macro_rules! vec {
        [$($args:tt)*] => {
            VecDebug(::std::vec![$($args)*])
        };
    }

    macro_rules! format {
        ($($args:tt)*) => {
            DisplayStr(::std::format!($($args)*))
        };
    }

    let mut sources = plaintext_sources!().map(|url| (url, vec![])).to_vec();

    sources.extend(json_sources!().map(|(source, key)| {
        (
            source,
            vec![format!(
                r#"ProcessStep::Json {{ key: "{}".into() }}"#,
                key.escape_debug()
            )],
        )
    }));

    file.write_all(format!("{sources:?}").0.as_bytes()).await?;

    file.flush().await
}

async fn generate_dispatcher() -> io::Result<()> {
    macro_rules! get_var {
        ($lit: literal) => {
            env::var($lit).map_err(|e| io::Error::other(format!(concat!($lit, " {err}"), err = e)))
        };
    }

    println!("cargo::rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    if get_var!("CARGO_CFG_TARGET_OS")? == "linux" {
        println!("cargo::rerun-if-changed=modules/linux-dispatcher");
        println!("cargo::rerun-if-changed=src/network_listener/linux/dispatcher");

        let target = get_var!("TARGET")?;
        let target = target.trim();

        Command::new("cargo")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .args(["build", "--profile", "linux-dispatcher", "--target", target])
            .current_dir("./modules/linux-dispatcher")
            .status()
            .await?
            .success()
            .then_some(())
            .ok_or_else(|| io::Error::other("failed to run dispatcher build command"))?;

        let target_path = {
            let path = format!(
                "./modules/linux-dispatcher/target/{target}/linux-dispatcher/linux-dispatcher"
            );

            Command::new("upx")
                .args(["--best", &*path])
                .status()
                .await?
                .success()
                .then_some(())
                .ok_or_else(|| {
                    io::Error::other("failed to pack linux-dispatcher, make sure upx is installed")
                })?;

            eprintln!(
                "{:?}",
                std::fs::read_dir(format!("./modules/linux-dispatcher/target/{target}"))
                    .and_then(|x| x.collect::<Result<Vec<_>, _>>())
            );

            tokio::fs::try_exists(&path)
                .await?
                .then_some(path)
                .ok_or_else(|| io::Error::other("unable to find dispatcher binary"))?
        };

        tokio::fs::rename(target_path, "./src/network_listener/linux/dispatcher").await?;
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tokio::fs::create_dir_all("./default/gen").await.unwrap();

    println!("cargo::rerun-if-changed=default");
    try_join!(
        make_default_sources_toml(),
        make_default_sources_rs(),
        generate_dispatcher()
    )
    .unwrap();
}
