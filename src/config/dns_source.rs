use reqwest::Url;
use wasmer::TypedFunction;
use crate::entity::OwnedStr;


enum Process {
    Plaintext,
    Strip { prefix: Option<OwnedStr>, suffix: Option<OwnedStr> },
    Json { ip_key: OwnedStr },
    Wasm(TypedFunction<(Box<str>,), Box<str>>)
}

struct IpSource {
    url: Url,
    process: Process
}