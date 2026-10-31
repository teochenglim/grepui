//! grepui: a multi-tenant observability UI for GreptimeDB, in one Rust binary.
#![forbid(unsafe_code)]

pub mod api;
pub mod config;
pub mod error;
pub mod org;
pub mod playground;
pub mod probe;
pub mod query;
pub mod server;
pub mod ui;

/// Build version, stamped by `GREPUI_VERSION` at compile time.
pub const VERSION: &str = match option_env!("GREPUI_VERSION") {
    Some(v) => v,
    None => concat!("v", env!("CARGO_PKG_VERSION")),
};
