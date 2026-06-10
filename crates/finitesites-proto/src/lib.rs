//! Wire-level protocol types for Finite Sites.
//!
//! This crate owns everything both the server and the CLI must agree on:
//! nostr event encoding and signature verification, NIP-98 request
//! authorization, site name rules, publish manifests, request/response DTOs,
//! and the explicit limits that bound every loop and payload in the system.

pub mod dto;
pub mod event;
pub mod hex;
pub mod ids;
pub mod limits;
pub mod manifest;
pub mod names;
pub mod nip98;
pub mod npub;

pub use event::NostrEvent;
pub use manifest::{ManifestFile, PublishManifest};

use thiserror::Error;

/// Errors for decoding and validating wire-level values.
///
/// These are handled errors for expected-bad external input. Internal
/// contradictions in this crate are bugs and use assertions instead.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("invalid hex: {0}")]
    InvalidHex(&'static str),
    #[error("invalid npub: {0}")]
    InvalidNpub(&'static str),
    #[error("invalid event: {0}")]
    InvalidEvent(&'static str),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("invalid auth header: {0}")]
    InvalidAuthHeader(&'static str),
    #[error("auth rejected: {0}")]
    AuthRejected(&'static str),
    #[error("invalid site name: {0}")]
    InvalidSiteName(&'static str),
    #[error("invalid manifest: {0}")]
    InvalidManifest(&'static str),
}
