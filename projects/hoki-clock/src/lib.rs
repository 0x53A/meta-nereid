// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
pub mod model;
pub mod store;
pub const BUS: &str = "org.hoki.Clock1";
pub const PATH: &str = "/org/hoki/Clock1";
#[path = "../../shared/clock_client.rs"]
mod client;
pub use client::request;
pub fn duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}
