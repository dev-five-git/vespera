//! File-based route modules. `vespera!` scans this folder; each `pub mod`
//! must be declared so the generated router can reference the handlers.
pub mod catalog;
pub mod orders;
pub mod users;
