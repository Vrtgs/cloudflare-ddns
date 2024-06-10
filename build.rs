use std::fmt::{Debug, Display, Formatter, Write};
use std::io;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::process::Command;
use tokio::task::JoinHandle;
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
        ($($args:tt)*) => {
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
    if cfg!(target_os = "linux") {
        println!("cargo::rerun-if-changed=modules/linux-dispatcher");
        println!("cargo::rerun-if-changed=src/network_listener/linux/dispatcher");

        let status = Command::new("cargo")
            .args(["build", "--release"])
            .current_dir("./modules/linux-dispatcher")
            .status().await?;

        if !status.success() {
            return Err(io::Error::other("failed to run dispatcher build command"))
        }

        let [no_prefix, gnu] = ["", "x86_64-unknown-linux-gnu/"].map(|infix|{
            let path = format!("./modules/linux-dispatcher/target/{infix}release/linux-dispatcher");
            let path = PathBuf::from(path);
            path.exists().then(|| {
                tokio::task::spawn_blocking(move || {
                    path.metadata()?.modified().map(|time| (time, path))
                })
            })
        });

        let map_await = |opt: Option<JoinHandle<_>>| async {
            match opt {
                Some(x) => Some(x.await),
                None => None,
            }
        };

        let [no_prefix, gnu] = [map_await(no_prefix).await, map_await(gnu).await];

        let flat_res = |opt: Option<_>| -> io::Result<_> {
            opt.transpose()?.transpose()
        };
        let times = [flat_res(no_prefix)?, flat_res(gnu)?];

        let x = match times {
            [Some((t1, p1)), Some((t2, p2))] => (t1, p1).max((t2, p2)).1,
            [Some((_, p)), None] => p,
            [None, Some((_, p))] => p,
            [None, None] => {
                println!("cargo::warning=Couldn't find daemon dispatcher");
                panic!("Couldn't find dispatcher")
            },
        };


        tokio::fs::rename(
            x,
            "./src/network_listener/linux/dispatcher"
        ).await?;
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
    ).unwrap();
}
