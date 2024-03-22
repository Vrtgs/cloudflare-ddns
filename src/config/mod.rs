use std::sync::Arc;
use once_cell::sync::Lazy;
use tokio::sync::RwLock;
use url::Url;
use crate::config::ip_source::{IpSource, Process};

mod ip_source;
mod listener;


pub struct Config {
    pub ip_sources: Sources
}


macro_rules! plain_text_array {
    ([$($domain: expr),* $(,)?]) => {{
        Arc::new([$(IpSource {
            url: Url::parse($domain).expect("Bad domain"),
            process: Process {
                steps: Arc::new([])
            }
        }),*])
    }};
}

pub use crate::config::ip_source::GetIpError;

pub static CONFIG: Lazy<RwLock<Config>> = Lazy::new(|| RwLock::new({
    Config {
        ip_sources: plain_text_array!([
            "https://checkip.amazonaws.com/",
            "https://api.ipify.org/",
            "https://ipv4.icanhazip.com/",
            "https://4.ident.me/",
            "https://v4.tnedi.me/",
            "https://v4.ipv6-test.com/api/myip.php",
            "https://myip.dnsomatic.com/",
            "https://ipinfo.io/ip",
            "https://ipv4.nsupdate.info/myip",
            "https://dynamic.zoneedit.com/checkip.html",
        ]),
    }
}));