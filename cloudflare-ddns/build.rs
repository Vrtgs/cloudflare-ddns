use anyhow::ensure;
use artifact_dependency::{CrateType, Profile};
use std::fmt::{Debug, Display, Formatter, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::{env, io};
use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::try_join;

macro_rules! plaintext_sources {
    () => {
        include!("includes/plaintext_sources")
    };
}

macro_rules! json_sources {
    () => {
        include!("includes/json_sources")
    };
}

macro_rules! get_var {
    ($lit: literal) => {{
        println!("cargo::rerun-if-env-changed={}", $lit);
        env::var($lit).map_err(|e| io::Error::other(format!(concat!($lit, " {err}"), err = e)))
    }};
}

static OUT_DIR: LazyLock<&Path> = LazyLock::new(|| {
    let boxed = PathBuf::from(get_var!("OUT_DIR").unwrap()).into_boxed_path();

    Box::leak(boxed)
});

async fn make_default_sources_toml() -> anyhow::Result<()> {
    let mut data = String::new();

    let plain_sources = plaintext_sources!();
    for (source, ip_type) in plain_sources {
        writeln!(data, r#"["{source}"]"#)?;
        writeln!(data, "type = \"{ip_type}\"")?;
        writeln!(data, "steps = [\"Plaintext\"]\n")?;
    }

    let plain_sources = json_sources!();
    for (source, key, ip_type) in plain_sources {
        writeln!(data, r#"["{source}"]"#)?;
        writeln!(data, "type = \"{ip_type}\"")?;
        writeln!(data, "steps = [{{ Json = {{ key = '{key}' }} }}]\n")?;
    }

    tokio::fs::write(OUT_DIR.join("sources.toml"), data.trim()).await?;
    Ok(())
}

async fn make_default_sources_rs() -> anyhow::Result<()> {
    let mut file = BufWriter::new(File::create(OUT_DIR.join("sources.array")).await?);

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

    #[derive(Copy, Clone)]
    enum DisplayIpType {
        Any,
        V6,
        V4,
    }

    impl DisplayIpType {
        fn parse(str: &str) -> Self {
            match str {
                "any" => Self::Any,
                "v4" => Self::V4,
                "v6" => Self::V6,
                _ => panic!("unknown ip type {str}"),
            }
        }
    }

    impl Debug for DisplayIpType {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            let str = match self {
                DisplayIpType::Any => "IpType::Any",
                DisplayIpType::V6 => "IpType::V6",
                DisplayIpType::V4 => "IpType::V4",
            };

            f.write_str(str)
        }
    }

    macro_rules! vec {
        [$($args:tt)*] => {
            VecDebug(::std::vec![$($args)*])
        };
    }

    let make_url = |url: &str| {
        let url = url::Url::parse(url).unwrap();
        let url = url.as_str();
        DisplayStr(format!(
            "::url::Url::parse({url:?}).unwrap_or_else(|_| {{ crate::abort_unreachable!(\"malformed url: '{url}'\") }})"
        ))
    };

    let mut sources = plaintext_sources!()
        .map(|(url, ip_type)| (make_url(url), Some(DisplayIpType::parse(ip_type)), vec![]))
        .to_vec();

    sources.extend(json_sources!().map(|(source, key, ip_type)| {
        (
            make_url(source),
            Some(DisplayIpType::parse(ip_type)),
            vec![DisplayStr(format!(
                r#"ProcessStep::Json {{ key: "{}".into() }}"#,
                key.escape_default()
            ))],
        )
    }));

    file.write_all(format!("{sources:?}").as_bytes()).await?;

    file.flush().await?;

    Ok(())
}

async fn generate_dispatcher() -> anyhow::Result<()> {
    if get_var!("CARGO_CFG_TARGET_OS")? == "linux" {
        println!("cargo::rerun-if-changed=/linux_dispatcher");
        println!("cargo::rerun-if-changed=src/network/linux/dispatcher");

        let target = get_var!("TARGET")?;

        // Cargo sets CARGO_CFG_DEBUG_ASSERTIONS to "true" or "false"
        let debug_assertions = get_var!("CARGO_CFG_DEBUG_ASSERTIONS")
            .map(|v| v == "true")
            .unwrap_or(false);

        tokio::task::spawn_blocking(move || {
            let artifact = artifact_dependency::ArtifactDependency::builder()
                .crate_name("linux_dispatcher")
                .artifact_type(CrateType::Executable)
                .profile(match debug_assertions {
                    false => Profile::Other("linux-dispatcher".into()),
                    true => Profile::Dev,
                })
                .target_name(target.trim())
                .build_always(true)
                .build_missing(false)
                .build()
                .build()
                .map_err(io::Error::other)?;

            assert_eq!(
                artifact.package.authors,
                ["IS", "LINUX", "DISPATCH", "ARTIFACT"]
            );

            std::fs::copy(artifact.path, OUT_DIR.join("./dispatcher-bin")).unwrap();

            Ok::<_, io::Error>(())
        })
        .await??;
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    ensure!(
        std::fs::metadata("./includes")?.is_dir(),
        "no includes directory"
    );

    println!("cargo::rerun-if-changed=default");
    try_join!(
        make_default_sources_toml(),
        make_default_sources_rs(),
        generate_dispatcher()
    )?;

    Ok(())
}
