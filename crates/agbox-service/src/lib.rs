//! Project-scoped application boundary shared by future transports.
pub mod app;
pub mod daemon;
pub mod health;
pub mod ipc;
pub mod logging;

pub use app::{
    ApplicationService, EvidenceReader, RequestActor, RequestScope, ServiceError, StoreWriter,
    WorkReader,
};
pub use daemon::{Components, Daemon, DaemonError};
pub use health::{DaemonHealth, DaemonHealthSnapshot, ProcessMemorySampler};
