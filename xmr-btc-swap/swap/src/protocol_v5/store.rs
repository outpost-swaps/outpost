use crate::database::SqliteDatabase;
use crate::protocol_v5::db;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Constructed but not necessarily published Monero lock, persisted before
/// broadcast so a restart can republish the exact same transaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingMoneroLock {
    pub tx_hash: String,
    pub tx_blob: String,
    pub tx_metadata: String,
    pub tx_key: String,
}

#[async_trait]
pub trait ProtocolV5Store: Send + Sync {
    async fn load_bob(&self, swap_id: Uuid) -> Result<Option<db::bob::Bob>>;
    async fn store_bob(&self, swap_id: Uuid, state: &db::bob::Bob) -> Result<()>;
    async fn load_alice(&self, swap_id: Uuid) -> Result<Option<db::alice::Alice>>;
    async fn store_alice(&self, swap_id: Uuid, state: &db::alice::Alice) -> Result<()>;
    async fn store_alice_peer_id(&self, swap_id: Uuid, peer_id: &str) -> Result<()>;
    async fn load_alice_peer_id(&self, swap_id: Uuid) -> Result<Option<String>>;
    async fn load_pending_monero_lock(&self, swap_id: Uuid) -> Result<Option<PendingMoneroLock>>;
    async fn load_pending_monero_refund(&self, swap_id: Uuid) -> Result<Option<PendingMoneroLock>>;
    async fn store_pending_monero_refund(
        &self,
        swap_id: Uuid,
        refund: &PendingMoneroLock,
    ) -> Result<()>;
    async fn store_pending_monero_lock(
        &self,
        swap_id: Uuid,
        lock: &PendingMoneroLock,
    ) -> Result<()>;
    async fn reserve_lock_inputs(
        &self,
        reservation_id: Uuid,
        outpoints: &[::bitcoin::OutPoint],
    ) -> Result<bool>;
    async fn rekey_lock_inputs(&self, reservation_id: Uuid, swap_id: Uuid) -> Result<()>;
    async fn release_lock_inputs(&self, swap_id: Uuid) -> Result<()>;
    async fn release_lock_input_reservation(&self, reservation_id: Uuid) -> Result<()>;
}

#[async_trait]
impl ProtocolV5Store for SqliteDatabase {
    async fn load_bob(&self, swap_id: Uuid) -> Result<Option<db::bob::Bob>> {
        self.load_protocol_v5_state(swap_id, "bob").await
    }

    async fn store_bob(&self, swap_id: Uuid, state: &db::bob::Bob) -> Result<()> {
        self.store_protocol_v5_state(swap_id, "bob", state).await
    }

    async fn load_alice(&self, swap_id: Uuid) -> Result<Option<db::alice::Alice>> {
        self.load_protocol_v5_state(swap_id, "alice").await
    }

    async fn store_alice(&self, swap_id: Uuid, state: &db::alice::Alice) -> Result<()> {
        self.store_protocol_v5_state(swap_id, "alice", state).await
    }

    async fn store_alice_peer_id(&self, swap_id: Uuid, peer_id: &str) -> Result<()> {
        self.store_protocol_v5_state(swap_id, "alice_peer", &peer_id.to_string())
            .await
    }

    async fn load_alice_peer_id(&self, swap_id: Uuid) -> Result<Option<String>> {
        self.load_protocol_v5_state(swap_id, "alice_peer").await
    }

    async fn load_pending_monero_lock(&self, swap_id: Uuid) -> Result<Option<PendingMoneroLock>> {
        self.load_protocol_v5_state(swap_id, "alice_monero_lock")
            .await
    }

    async fn load_pending_monero_refund(
        &self,
        swap_id: Uuid,
    ) -> Result<Option<PendingMoneroLock>> {
        self.load_protocol_v5_state(swap_id, "alice_monero_refund")
            .await
    }

    async fn store_pending_monero_refund(
        &self,
        swap_id: Uuid,
        refund: &PendingMoneroLock,
    ) -> Result<()> {
        self.store_protocol_v5_state(swap_id, "alice_monero_refund", refund)
            .await
    }

    async fn store_pending_monero_lock(
        &self,
        swap_id: Uuid,
        lock: &PendingMoneroLock,
    ) -> Result<()> {
        self.store_protocol_v5_state(swap_id, "alice_monero_lock", lock)
            .await
    }

    async fn reserve_lock_inputs(
        &self,
        reservation_id: Uuid,
        outpoints: &[::bitcoin::OutPoint],
    ) -> Result<bool> {
        self.reserve_protocol_v5_lock_inputs(reservation_id, outpoints)
            .await
    }

    async fn rekey_lock_inputs(&self, reservation_id: Uuid, swap_id: Uuid) -> Result<()> {
        self.rekey_protocol_v5_lock_inputs(reservation_id, swap_id)
            .await
    }

    async fn release_lock_inputs(&self, swap_id: Uuid) -> Result<()> {
        self.release_protocol_v5_lock_inputs(swap_id).await
    }

    async fn release_lock_input_reservation(&self, reservation_id: Uuid) -> Result<()> {
        self.release_protocol_v5_lock_input_reservation(reservation_id)
            .await
    }
}
