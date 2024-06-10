#![no_main]

use std::os::unix::net::UnixStream;

const SOCKET_PATH: &str = include_str!("../../../src/network_listener/linux/socket-path");

#[no_mangle]
fn main() -> i32 {
    let _ = UnixStream::connect(SOCKET_PATH);
    0
}