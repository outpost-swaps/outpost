//! Minimal Monero wallet boundary for the protocol-5 state machine.
//!
//! Eigenwallet's `swap-machine` only needs `Wallets::has_received_outputs`.
//! The boundary deliberately uses raw key bytes and heights so the Outpost app
//! can implement it with its Rust `monero-wallet-ng` service without importing
//! Eigenwallet's C++ `monero-sys` wallet or the rest of the protocol crate graph.

use anyhow::Result;
use backoff::ExponentialBackoff;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub trait HasReceivedOutputs: Send + Sync {
    fn has_received_outputs<'a>(
        &'a self,
        public_spend_key: [u8; 32],
        private_view_key: [u8; 32],
        start_height: u64,
        target_tip: Option<u64>,
        inner_retry: Option<ExponentialBackoff>,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;
}

pub struct Wallets {
    inner: Arc<dyn HasReceivedOutputs>,
}

impl Wallets {
    pub fn new(inner: Arc<dyn HasReceivedOutputs>) -> Self {
        Self { inner }
    }

    pub async fn has_received_outputs(
        &self,
        public_spend_key: [u8; 32],
        private_view_key: [u8; 32],
        start_height: u64,
        target_tip: Option<u64>,
        inner_retry: Option<ExponentialBackoff>,
    ) -> Result<bool> {
        self.inner
            .has_received_outputs(
                public_spend_key,
                private_view_key,
                start_height,
                target_tip,
                inner_retry,
            )
            .await
    }
}
