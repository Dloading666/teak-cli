//! Grok Build-only collaboration core.
//!
//! PTY/session discovery and transport live outside this module.  This module
//! accepts only members and generations already approved by the user-owned
//! team configuration.

pub mod broker;
pub mod grok;
pub mod helper;
pub mod management;
pub mod model;
pub mod protocol;
pub mod runtime;
pub mod service;
pub mod store;

pub use service::CollaborationService;
