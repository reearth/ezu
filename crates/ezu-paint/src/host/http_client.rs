//! The HTTP client the tile-pyramid fetchers (`raster`, `dem`) share,
//! built on the first request rather than when the registry is.
//!
//! Building a client loads the system's root certificates, and with none
//! installed (a bare Linux container) the build fails — `Client::new()`
//! and `Client::default()` panic on it. A style whose sources are all
//! local never sends a request, so it must not need certificates to start.

use std::sync::{Arc, OnceLock};

use reqwest::Client;

/// A client shared between clones, built on first [`get`](Self::get).
#[derive(Clone, Default)]
pub(super) struct LazyClient(Arc<OnceLock<Result<Client, String>>>);

impl LazyClient {
    /// The client, or why it could not be built.
    pub(super) fn get(&self) -> Result<&Client, String> {
        self.0
            .get_or_init(|| {
                Client::builder()
                    .user_agent(concat!("ezu/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .map_err(|e| format!("building the HTTP client: {e}"))
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}
