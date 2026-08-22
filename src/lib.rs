#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod cli;
pub mod exec;
pub mod tool;
pub mod transport;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
