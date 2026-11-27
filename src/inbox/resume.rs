use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

pub fn report(pid: u32, seconds: u64, token: &str) -> String {
    let body = format!(
        "pid={pid}:time={seconds}:argv={},{}",
        STANDARD.encode("rtm"),
        STANDARD.encode("inbox")
    );
    let mac = Sha256::digest(format!("{body}:{token}"));
    format!("\x1b]7577;{body}:mac={mac:x}\x1b\\")
}
