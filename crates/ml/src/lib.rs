//! `photocraft-ml`: inference back-end abstraction for AI-assisted editing.
//!
//! Provides the [`InferenceBackend`] trait and two concrete implementations:
//!
//! - [`LocalBackend`] – talks to a user-run server (Ollama, llama.cpp, any
//!   server that speaks the Ollama-compatible REST API) over plain HTTP.
//! - [`ClassicalFallback`] – a pure-Rust stub that returns
//!   [`MlError::Unsupported`]; the engine substitutes the classical
//!   PatchMatch/Poisson inpaint when this is the active backend.
//!
//! # Wasm
//! On `wasm32` neither `LocalBackend` nor an HTTP client is compiled in
//! (the `native` Cargo feature controls this).  All inference returns
//! `Err(MlError::Unsupported)` on wasm.  Callers must fall back to a
//! classical algorithm.
//!
//! # Layering
//! This crate is at L4 (same as `tools`, `io`, `plugins`).  It never imports
//! egui, winit, or any platform implementation.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

pub mod backend;
pub mod error;
pub mod request;
pub mod task;

pub use backend::{ClassicalFallback, InferenceBackend, LocalBackend};
pub use error::MlError;
pub use request::{InferenceRequest, InferenceResult};
pub use task::Task;

