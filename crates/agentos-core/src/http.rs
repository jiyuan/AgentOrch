//! Shared HTTP client used by built-in tools and external storage backends.
//!
//! `reqwest::Client` already pools connections per host internally; using a
//! single process-wide instance lets the built-in `http` tool, the Qdrant
//! semantic index, and any future HTTP consumer share keep-alive connections
//! and TLS state.

use reqwest::Client;
use std::sync::OnceLock;
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) fn shared_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .user_agent(concat!("agentos-core/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builds with rustls + http2 features compiled in")
    })
}
