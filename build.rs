use tokio::fs::File;
use std::io;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::try_join;

macro_rules! plaintext_sources {
    () => {include!("./default/plaintext_sources")};
}

async fn make_default_sources_toml() -> io::Result<()> {
    let mut file = BufWriter::new(File::create("./default/gen/sources.toml").await?);
    
    let sources = plaintext_sources!();
    for (source, i) in sources.into_iter().zip(1..) {
        file.write_all(format!(r#"["{source}"]"#).as_bytes()).await?;
        file.write_all(b"\nsteps = [\"Plaintext\"]").await?;
        if i != sources.len() { file.write_all(b"\n\n").await? }
    }
    
    file.flush().await
}

async fn make_default_sources_rs() -> io::Result<()> {
    let mut file = BufWriter::new(File::create("./default/gen/sources.array").await?);

    let sources = plaintext_sources!()
        .map::<_, (&str, [(); 0])>(|url| (url, []));
    
    file.write_all(format!("{sources:?}").as_bytes()).await?;

    file.flush().await
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    try_join!(
        make_default_sources_toml(),
        make_default_sources_rs()
    ).unwrap();
}