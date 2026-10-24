pub mod config;
pub mod engine;
pub mod market;
pub mod protocol;
pub mod state;
pub mod transport;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as u64
}
