//! Project-scoped application boundary shared by future transports.
pub mod app;
pub mod daemon;
pub mod health;
pub mod ipc;
pub mod logging;
pub mod mcp;

pub use app::{
    ApplicationService, EvidenceReader, RequestActor, RequestScope, ServiceError, StoreWriter,
    WorkReader,
};
pub use daemon::{Components, Daemon, DaemonError};
pub use health::{DaemonHealth, DaemonHealthSnapshot, ProcessMemorySampler};
pub use mcp::{
    AppClient, ClientError, GetEvidenceInput, GetWorkInput, HandoffMcpServer, IpcAppClient,
    ListWorkInput, SearchWorkInput, WorkStatusInput, serve_stdio,
};
