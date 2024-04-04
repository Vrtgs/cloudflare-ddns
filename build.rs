use tokio::fs::File;
use std::io;
use std::fmt::{Debug, Display, Formatter, Write};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::try_join;

macro_rules! plaintext_sources {
    () => {include!("./default/plaintext_sources")};
}

macro_rules! json_sources {
    () => { include!("./default/json_sources") };
}

async fn make_default_sources_toml() -> io::Result<()> {
    let mut data = String::new();
    
    let plain_sources = plaintext_sources!();
    for source in plain_sources.into_iter() {
        writeln!(data, r#"["{source}"]"#).unwrap();
        writeln!(data, "steps = [\"Plaintext\"]\n").unwrap();
    }

    let plain_sources = json_sources!();
    for (source, key) in plain_sources.into_iter() {
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
    
    let mut sources = plaintext_sources!()
        .map(|url| (url, vec![]))
        .to_vec();
    
    sources.extend(json_sources!().map(|(source, key)| (source, vec![
        format!(r#"ProcessStep::Json {{ key: "{}".into() }}"#, key.escape_debug())
    ])));
    
    file.write_all(format!("{sources:?}").0.as_bytes()).await?;

    file.flush().await
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    try_join!(
        make_default_sources_toml(),
        make_default_sources_rs()
    ).unwrap();
}