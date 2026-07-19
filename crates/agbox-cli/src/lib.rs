//! Native setup and command boundary for agbox.

pub mod config;
pub mod init;
pub mod paths;
pub mod platform;

pub use init::{InitOptions, InitReport, Initializer};
pub use paths::AgboxPaths;
pub use platform::{Change, Platform, PlatformError, ServiceSpec};
