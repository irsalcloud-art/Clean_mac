//! MacClean: a storage analyzer, safe cache cleaner and system monitor for macOS.

#[cfg(not(target_os = "macos"))]
compile_error!("MacClean supports macOS only.");

pub mod cleaner;
pub mod cli;
pub mod errors;
pub mod monitor;
pub mod platform;
pub mod policy;
pub mod report;
pub mod scanner;
