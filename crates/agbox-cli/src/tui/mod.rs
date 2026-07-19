//! Bounded work-centered terminal state.

mod app;
pub mod event;
pub mod render;
pub mod terminal;

pub use app::{App, AppError, Effect, Focus, Message};
