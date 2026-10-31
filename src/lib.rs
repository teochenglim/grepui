//! grepui: a multi-tenant observability UI for GreptimeDB, in one Rust binary.
#![forbid(unsafe_code)]

pub mod admin;
pub mod alerts;
pub mod api;
pub mod config;
pub mod csv;
pub mod dash;
pub mod enrich;
pub mod error;
pub mod functions;
pub mod http;
pub mod layout;
pub mod org;
pub mod pipelines;
pub mod playground;
pub mod probe;
pub mod query;
pub mod redact;
pub mod server;
pub mod store;
pub mod ui;

/// Build version, stamped by `GREPUI_VERSION` at compile time.
pub const VERSION: &str = match option_env!("GREPUI_VERSION") {
    Some(v) => v,
    None => concat!("v", env!("CARGO_PKG_VERSION")),
};
