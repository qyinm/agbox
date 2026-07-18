//! Project-scoped application boundary shared by future transports.
pub mod app;

pub use app::{
    ApplicationService, EvidenceReader, RequestActor, RequestScope, ServiceError, StoreWriter,
    WorkReader,
};
