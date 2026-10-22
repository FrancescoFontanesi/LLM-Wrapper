//! Minimal protocol-speaking daemon double: answers every request with `Ok`. Used to measure the
//! client in isolation (`mockd /run/user/1000/llm-wrapper.v1.sock`).

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;

use llm_wrapper_protocol::frame::{self, Kind};
use llm_wrapper_protocol::Response;

fn main() {
    let path = std::env::args().nth(1).expect("usage: mockd SOCKET_PATH");
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    let reply = Response::ok().encode().expect("encode");
    for conn in listener.incoming().flatten() {
        let mut conn = conn;
        if frame::read(&mut conn, Kind::Request).is_ok() {
            let _ = conn.write_all(&reply);
        }
    }
}
