use crate::database::cipher::{is_key_check_plaintext, key_check_plaintext, STATE_CIPHER_PREFIX};
use crate::database::{StateCipher, Swap};
use crate::monero::{Address, TxHash};
use crate::protocol::{Database, State};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use libp2p::{Multiaddr, PeerId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::Sqlite;
use sqlx::{Pool, SqlitePool};
use std::path::Path;
use std::str::FromStr;
use time::OffsetDateTime;
use uuid::Uuid;

const META_KEY_ID: &str = "state_cipher_key_id";
const META_KEY_CHECK: &str = "state_cipher_key_check";

pub struct SqliteDatabase {
    pool: Pool<Sqlite>,
    cipher: Option<StateCipher>,
}

impl SqliteDatabase {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self>
    where
        Self: std::marker::Sized,
    {
        Self::open_with_cipher(path, None).await
    }

    pub async fn open_with_cipher(
        path: impl AsRef<Path>,
        cipher: Option<StateCipher>,
    ) -> Result<Self>
    where
        Self: std::marker::Sized,
    {
        let path_str = format!("sqlite:{}", path.as_ref().display());
        let pool = SqlitePool::connect(&path_str).await?;
        let mut sqlite = Self { pool, cipher };
        sqlite.run_migrations().await?;
        // A pre-registration reservation cannot survive an ASB restart: no
        // setup survives the process. Clear these so a crash cannot poison
        // every later attempt to spend the same deposit.
        sqlx::query("DELETE FROM protocol_v5_lock_inputs WHERE swap_id IS NULL")
            .execute(&sqlite.pool)
            .await?;
        sqlite.prepare_cipher().await?;
        super::restrict_database_permissions(path.as_ref())?;
        Ok(sqlite)
    }

    async fn run_migrations(&mut self) -> anyhow::Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }

    async fn prepare_cipher(&self) -> Result<()> {
        let Some(cipher) = &self.cipher else {
            return Ok(());
        };

        let stored_key_id: Option<String> =
            sqlx::query_scalar("SELECT value FROM db_meta WHERE key = ?")
                .bind(META_KEY_ID)
                .fetch_optional(&self.pool)
                .await?;

        if let Some(stored_key_id) = stored_key_id {
            if stored_key_id != cipher.key_id() {
                bail!("Swap database was encrypted with a different wallet seed");
            }
            let canary: Option<String> =
                sqlx::query_scalar("SELECT value FROM db_meta WHERE key = ?")
                    .bind(META_KEY_CHECK)
                    .fetch_optional(&self.pool)
                    .await?;
            let canary = canary.context("Encrypted swap database is missing its key check")?;
            let plaintext = cipher.decrypt(&canary)?;
            if !is_key_check_plaintext(&plaintext) {
                bail!("Swap database key check failed");
            }
            return Ok(());
        }

        let encrypted_state_exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM swap_states WHERE state LIKE ? LIMIT 1")
                .bind(format!("{STATE_CIPHER_PREFIX}%"))
                .fetch_optional(&self.pool)
                .await?;
        if encrypted_state_exists.is_some() {
            bail!("Swap database has encrypted states but no key metadata");
        }

        self.migrate_plaintext_states(cipher).await
    }

    async fn migrate_plaintext_states(&self, cipher: &StateCipher) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query("SELECT id, state FROM swap_states")
            .fetch_all(&mut *tx)
            .await?;

        for row in rows {
            use sqlx::Row;
            let id: i64 = row.try_get("id")?;
            let state: String = row.try_get("state")?;
            if state.starts_with(STATE_CIPHER_PREFIX) {
                continue;
            }
            let encrypted = cipher.encrypt(state.as_bytes())?;
            sqlx::query("UPDATE swap_states SET state = ? WHERE id = ?")
                .bind(encrypted)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }

        let protocol_v5_rows =
            sqlx::query("SELECT swap_id, role, state FROM protocol_v5_swap_states")
                .fetch_all(&mut *tx)
                .await?;
        for row in protocol_v5_rows {
            use sqlx::Row;
            let swap_id: String = row.try_get("swap_id")?;
            let role: String = row.try_get("role")?;
            let state: String = row.try_get("state")?;
            if state.starts_with(STATE_CIPHER_PREFIX) {
                continue;
            }
            let encrypted = cipher.encrypt(state.as_bytes())?;
            sqlx::query(
                "UPDATE protocol_v5_swap_states SET state = ? WHERE swap_id = ? AND role = ?",
            )
            .bind(encrypted)
            .bind(swap_id)
            .bind(role)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query("INSERT OR IGNORE INTO db_meta (key, value) VALUES (?, ?), (?, ?)")
            .bind(META_KEY_ID)
            .bind(cipher.key_id())
            .bind(META_KEY_CHECK)
            .bind(cipher.encrypt(key_check_plaintext())?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        if let Err(error) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await
        {
            tracing::warn!(%error, "Could not checkpoint the encrypted database WAL");
        }
        if let Err(error) = sqlx::query("VACUUM").execute(&self.pool).await {
            tracing::warn!(%error, "Could not vacuum the encrypted database");
        }
        Ok(())
    }

    fn encode_state(&self, state: &State) -> Result<String> {
        let serialized = serde_json::to_vec(&Swap::from(state.clone()))?;
        match &self.cipher {
            Some(cipher) => cipher.encrypt(&serialized),
            None => Ok(String::from_utf8(serialized)?),
        }
    }

    fn decode_state(&self, stored: &str) -> Result<Swap> {
        self.decode_value(stored)
    }

    fn encode_value<T: Serialize>(&self, value: &T) -> Result<String> {
        let serialized = serde_json::to_vec(value)?;
        match &self.cipher {
            Some(cipher) => cipher.encrypt(&serialized),
            None => Ok(String::from_utf8(serialized)?),
        }
    }

    fn decode_value<T: DeserializeOwned>(&self, stored: &str) -> Result<T> {
        let plaintext = match &self.cipher {
            Some(cipher) => {
                if !stored.starts_with(STATE_CIPHER_PREFIX) {
                    bail!("Encrypted swap database contains an unencrypted state row");
                }
                cipher.decrypt(stored)?
            }
            None => stored.as_bytes().to_vec(),
        };
        Ok(serde_json::from_slice(&plaintext)?)
    }

    /// Persist a protocol-5 state separately from the legacy `swap_states`
    /// table. This keeps the 5.0 state machine from changing the legacy
    /// `State`/JNI shape until cutover.
    pub async fn store_protocol_v5_state<T: Serialize>(
        &self,
        swap_id: Uuid,
        role: &str,
        state: &T,
    ) -> Result<()> {
        let state = self.encode_value(state)?;
        let now = OffsetDateTime::now_utc().to_string();

        sqlx::query(
            r#"
            INSERT INTO protocol_v5_swap_states (swap_id, role, state, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(swap_id, role) DO UPDATE SET
                state = excluded.state,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(swap_id.to_string())
        .bind(role)
        .bind(state)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// List all protocol-5 states for a role, newest first.
    pub async fn all_protocol_v5_states(
        &self,
        role: &str,
    ) -> Result<Vec<(Uuid, serde_json::Value, String)>> {
        use sqlx::Row;

        let rows = sqlx::query(
            "SELECT swap_id, state, updated_at FROM protocol_v5_swap_states WHERE role = ? ORDER BY updated_at DESC",
        )
        .bind(role)
        .fetch_all(&self.pool)
        .await?;

        let mut states = Vec::with_capacity(rows.len());
        for row in rows {
            let swap_id: String = row.try_get("swap_id")?;
            let state: String = row.try_get("state")?;
            let updated_at: String = row.try_get("updated_at")?;
            let value = self.decode_value(&state)?;
            states.push((Uuid::parse_str(&swap_id)?, value, updated_at));
        }

        Ok(states)
    }

    /// Reserve the outpoints of a protocol-5 lock transaction.
    ///
    /// Returns `false` if any outpoint is already reserved by another
    /// reservation or swap. The reservation is persisted so restarts keep
    /// conflicting setups apart.
    pub async fn reserve_protocol_v5_lock_inputs(
        &self,
        reservation_id: Uuid,
        outpoints: &[::bitcoin::OutPoint],
    ) -> Result<bool> {
        let now = OffsetDateTime::now_utc().to_string();
        let mut tx = self.pool.begin().await?;

        for outpoint in outpoints {
            let outpoint = outpoint.to_string();
            let result = sqlx::query(
                "INSERT INTO protocol_v5_lock_inputs (reservation_id, swap_id, outpoint, created_at) VALUES (?, NULL, ?, ?) ON CONFLICT(outpoint) DO NOTHING",
            )
            .bind(reservation_id.to_string())
            .bind(&outpoint)
            .bind(&now)
            .execute(&mut *tx)
            .await?;

            if result.rows_affected() == 0 {
                let owner: String = sqlx::query_scalar(
                    "SELECT reservation_id FROM protocol_v5_lock_inputs WHERE outpoint = ?",
                )
                .bind(&outpoint)
                .fetch_one(&mut *tx)
                .await?;
                if owner != reservation_id.to_string() {
                    tx.rollback().await?;
                    return Ok(false);
                }
            }
        }

        tx.commit().await?;
        Ok(true)
    }

    pub async fn rekey_protocol_v5_lock_inputs(
        &self,
        reservation_id: Uuid,
        swap_id: Uuid,
    ) -> Result<()> {
        sqlx::query("UPDATE protocol_v5_lock_inputs SET swap_id = ? WHERE reservation_id = ?")
            .bind(swap_id.to_string())
            .bind(reservation_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn release_protocol_v5_lock_inputs(&self, swap_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM protocol_v5_lock_inputs WHERE swap_id = ?")
            .bind(swap_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn release_protocol_v5_lock_input_reservation(
        &self,
        reservation_id: Uuid,
    ) -> Result<()> {
        sqlx::query("DELETE FROM protocol_v5_lock_inputs WHERE reservation_id = ? AND swap_id IS NULL")
            .bind(reservation_id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Load a protocol-5 state persisted by [`Self::store_protocol_v5_state`].
    pub async fn load_protocol_v5_state<T: DeserializeOwned>(
        &self,
        swap_id: Uuid,
        role: &str,
    ) -> Result<Option<T>> {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT state FROM protocol_v5_swap_states WHERE swap_id = ? AND role = ?",
        )
        .bind(swap_id.to_string())
        .bind(role)
        .fetch_optional(&self.pool)
        .await?;

        stored
            .map(|stored| self.decode_value(&stored))
            .transpose()
    }
}

#[async_trait]
impl Database for SqliteDatabase {
    async fn insert_peer_id(&self, swap_id: Uuid, peer_id: PeerId) -> Result<()> {
        let mut conn = self.pool.acquire().await?;

        let swap_id = swap_id.to_string();
        let peer_id = peer_id.to_string();

        sqlx::query!(
            r#"
        insert into peers (
            swap_id,
            peer_id
            ) values (?, ?);
        "#,
            swap_id,
            peer_id
        )
        .execute(&mut *conn)
        .await?;

        Ok(())
    }

    async fn get_peer_id(&self, swap_id: Uuid) -> Result<PeerId> {
        let mut conn = self.pool.acquire().await?;

        let swap_id = swap_id.to_string();

        let row = sqlx::query!(
            r#"
        SELECT peer_id
        FROM peers
        WHERE swap_id = ?
        "#,
            swap_id
        )
        .fetch_one(&mut *conn)
        .await?;

        let peer_id = PeerId::from_str(&row.peer_id)?;
        Ok(peer_id)
    }

    async fn insert_redeem_sweep_txid(&self, swap_id: Uuid, txid: TxHash) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            r#"
            INSERT INTO redeem_sweeps (swap_id, txid, created_at)
            VALUES (?, ?, datetime('now'))
            ON CONFLICT(swap_id) DO UPDATE SET
                txid = excluded.txid,
                created_at = excluded.created_at
            "#,
        )
        .bind(swap_id.to_string())
        .bind(txid.0)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    async fn get_redeem_sweep_txid(&self, swap_id: Uuid) -> Result<Option<TxHash>> {
        let mut conn = self.pool.acquire().await?;
        let txid: Option<String> =
            sqlx::query_scalar("SELECT txid FROM redeem_sweeps WHERE swap_id = ?")
                .bind(swap_id.to_string())
                .fetch_optional(&mut *conn)
                .await?;
        Ok(txid.map(TxHash))
    }

    async fn insert_monero_address(&self, swap_id: Uuid, address: Address) -> Result<()> {
        let mut conn = self.pool.acquire().await?;

        let swap_id = swap_id.to_string();
        let address = address.to_string();

        sqlx::query!(
            r#"
        insert into monero_addresses (
            swap_id,
            address
            ) values (?, ?);
        "#,
            swap_id,
            address
        )
        .execute(&mut *conn)
        .await?;

        Ok(())
    }

    async fn get_monero_address(&self, swap_id: Uuid) -> Result<Address> {
        let mut conn = self.pool.acquire().await?;

        let swap_id = swap_id.to_string();

        let row = sqlx::query!(
            r#"
        SELECT address
        FROM monero_addresses
        WHERE swap_id = ?
        "#,
            swap_id
        )
        .fetch_one(&mut *conn)
        .await?;

        let address = row.address.parse()?;

        Ok(address)
    }

    async fn insert_address(&self, peer_id: PeerId, address: Multiaddr) -> Result<()> {
        let mut conn = self.pool.acquire().await?;

        let peer_id = peer_id.to_string();
        let address = address.to_string();

        sqlx::query!(
            r#"
        insert into peer_addresses (
            peer_id,
            address
            ) values (?, ?);
        "#,
            peer_id,
            address
        )
        .execute(&mut *conn)
        .await?;

        Ok(())
    }

    async fn get_addresses(&self, peer_id: PeerId) -> Result<Vec<Multiaddr>> {
        let mut conn = self.pool.acquire().await?;

        let peer_id = peer_id.to_string();

        let rows = sqlx::query!(
            r#"
        SELECT address
        FROM peer_addresses
        WHERE peer_id = ?
        "#,
            peer_id,
        )
        .fetch_all(&mut *conn)
        .await?;

        let addresses = rows
            .iter()
            .map(|row| {
                let multiaddr = Multiaddr::from_str(&row.address)?;
                Ok(multiaddr)
            })
            .collect::<Result<Vec<Multiaddr>>>();

        addresses
    }

    async fn insert_latest_state(&self, swap_id: Uuid, state: State) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        let entered_at = OffsetDateTime::now_utc();

        let swap_id = swap_id.to_string();
        let swap = self.encode_state(&state)?;
        let entered_at = entered_at.to_string();

        sqlx::query!(
            r#"
            insert into swap_states (
                swap_id,
                entered_at,
                state
                ) values (?, ?, ?);
        "#,
            swap_id,
            entered_at,
            swap
        )
        .execute(&mut *conn)
        .await?;

        Ok(())
    }

    async fn get_state(&self, swap_id: Uuid) -> Result<State> {
        let mut conn = self.pool.acquire().await?;
        let swap_id = swap_id.to_string();
        let row = sqlx::query!(
            r#"
           SELECT state
           FROM swap_states
           WHERE swap_id = ?
           ORDER BY id desc
           LIMIT 1;

        "#,
            swap_id
        )
        .fetch_all(&mut *conn)
        .await?;

        let row = row
            .first()
            .context(format!("No state in database for swap: {}", swap_id))?;
        let swap = self.decode_state(&row.state)?;

        Ok(swap.into())
    }

    async fn get_all_states(&self, swap_id: Uuid) -> Result<Vec<State>> {
        use sqlx::Row;

        let mut conn = self.pool.acquire().await?;
        let swap_id = swap_id.to_string();
        let rows = sqlx::query("SELECT state FROM swap_states WHERE swap_id = ? ORDER BY id asc;")
            .bind(&swap_id)
            .fetch_all(&mut *conn)
            .await?;

        rows.iter()
            .map(|row| {
                let state: String = row.try_get("state")?;
                let swap = self.decode_state(&state)?;
                Ok(swap.into())
            })
            .collect()
    }

    async fn all(&self) -> Result<Vec<(Uuid, State)>> {
        let mut conn = self.pool.acquire().await?;
        let rows = sqlx::query!(
            r#"
           SELECT swap_id, state
           FROM swap_states
           WHERE id IN (
               SELECT max(id)
               FROM swap_states
               GROUP BY swap_id
           )
        "#
        )
        .fetch_all(&mut *conn)
        .await?;

        let result = rows
            .iter()
            .map(|row| {
                let swap_id = Uuid::from_str(&row.swap_id)?;
                let state = State::from(self.decode_state(&row.state)?);
                Ok((swap_id, state))
            })
            .collect::<Result<Vec<(Uuid, State)>>>();

        result
    }
}

impl SqliteDatabase {
    /// Latest state per swap together with the timestamp it was persisted,
    /// for the history JNI endpoints (date column). Timestamps are UTC.
    pub async fn all_with_dates(&self) -> Result<Vec<(Uuid, State, String)>> {
        use sqlx::Row;

        let mut conn = self.pool.acquire().await?;
        let rows = sqlx::query(
            r#"
           SELECT swap_id, state, entered_at
           FROM swap_states
           WHERE id IN (
               SELECT max(id)
               FROM swap_states
               GROUP BY swap_id
           )
        "#,
        )
        .fetch_all(&mut *conn)
        .await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let swap_id: String = row.try_get("swap_id")?;
            let state: String = row.try_get("state")?;
            let entered_at: String = row.try_get("entered_at")?;

            let swap_id = match Uuid::from_str(&swap_id) {
                Ok(swap_id) => swap_id,
                Err(error) => {
                    tracing::warn!(%swap_id, %error, "Skipping history row with invalid swap id");
                    continue;
                }
            };
            let state = match self.decode_state(&state) {
                Ok(state) => State::from(state),
                Err(error) => {
                    tracing::warn!(%swap_id, %error, "Skipping history row with corrupt state");
                    continue;
                }
            };
            result.push((swap_id, state, format_history_date(&entered_at)));
        }
        Ok(result)
    }
}

/// Format a persisted `entered_at` timestamp for the history date column.
///
/// `OffsetDateTime`'s default `Display` does not zero-pad the hour
/// (`2026-10-22 4:06:27...`), so slicing a fixed number of characters leaves a
/// trailing colon on one-digit hours.
pub fn format_history_date(entered_at: &str) -> String {
    match entered_at.split_once(' ') {
        Some((date, time)) => {
            let mut parts = time.split(':');
            match (parts.next(), parts.next()) {
                (Some(hour), Some(minute)) => format!("{date} {hour:0>2}:{minute}"),
                _ => entered_at.to_owned(),
            }
        }
        None => entered_at.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_date_pads_one_digit_hours_without_a_trailing_colon() {
        assert_eq!(
            format_history_date("2026-10-22 4:06:27.657984 +00:00:00"),
            "2026-10-22 04:06"
        );
        assert_eq!(
            format_history_date("2026-10-20 14:30:42.032066 +00:00:00"),
            "2026-10-20 14:30"
        );
        assert_eq!(format_history_date("not-a-date"), "not-a-date");
    }

    #[test]
    fn v5_history_date_formats_updated_at_timestamp() {
        assert_eq!(
            format_history_date("2026-11-03 17:02:26.967286 +00:00:00"),
            "2026-11-03 17:02"
        );
    }

    #[tokio::test]
    async fn protocol_v5_alice_states_are_returned_for_asb_history() -> Result<()> {
        #[derive(Debug, serde::Deserialize, serde::Serialize)]
        struct FakeState {
            step: u8,
        }

        let dir = tempdir()?;
        let path = dir.path().join("protocol5-asb-history.db");
        File::create(&path)?;
        let db = SqliteDatabase::open(&path).await?;
        let swap_id = Uuid::new_v4();

        db.store_protocol_v5_state(swap_id, "alice", &FakeState { step: 3 })
            .await?;
        db.store_protocol_v5_state(swap_id, "alice_monero_lock", &FakeState { step: 4 })
            .await?;

        let alice = db.all_protocol_v5_states("alice").await?;
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].0, swap_id);
        assert_eq!(alice[0].1["step"], 3);
        assert!(db.all_protocol_v5_states("bob").await?.is_empty());

        Ok(())
    }

    use crate::bitcoin::{Address, Amount};
    use crate::protocol::alice::AliceState;
    use crate::protocol::bob::BobState;
    use std::fs::File;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_insert_and_load_state() {
        let db = setup_test_db().await.unwrap();

        let state_1 = State::Alice(AliceState::BtcRedeemed);
        let swap_id_1 = Uuid::new_v4();

        db.insert_latest_state(swap_id_1, state_1).await.unwrap();

        let state_1 = State::Alice(AliceState::BtcRedeemed);

        db.insert_latest_state(swap_id_1, state_1.clone())
            .await
            .unwrap();

        let state_1_loaded = db.get_state(swap_id_1).await.unwrap();

        assert_eq!(state_1, state_1_loaded);
    }

    #[tokio::test]
    async fn started_bob_state_is_persisted_and_visible_in_history() -> Result<()> {
        let db = setup_test_db().await?;
        let swap_id = Uuid::new_v4();
        let state = State::Bob(BobState::Started {
            btc_amount: Amount::from_sat(123_456),
            change_address: Address::from_str("bcrt1q08pfqpsyrt7acllzyjm8q5qsz5capvyahm49rw")?.assume_checked(),
        });

        db.insert_latest_state(swap_id, state.clone()).await?;

        assert_eq!(db.get_state(swap_id).await?, state);
        assert!(db.all().await?.contains(&(swap_id, state)));
        Ok(())
    }

    #[tokio::test]
    async fn test_retrieve_all_latest_states() {
        let db = setup_test_db().await.unwrap();

        let state_1 = State::Alice(AliceState::BtcRedeemed);
        let state_2 = State::Alice(AliceState::BtcPunished);
        let state_3 = State::Alice(AliceState::SafelyAborted);
        let state_4 = State::Bob(BobState::SafelyAborted);
        let swap_id_1 = Uuid::new_v4();
        let swap_id_2 = Uuid::new_v4();

        db.insert_latest_state(swap_id_1, state_1.clone())
            .await
            .unwrap();
        db.insert_latest_state(swap_id_1, state_2.clone())
            .await
            .unwrap();
        db.insert_latest_state(swap_id_1, state_3.clone())
            .await
            .unwrap();
        db.insert_latest_state(swap_id_2, state_4.clone())
            .await
            .unwrap();

        let latest_loaded = db.all().await.unwrap();

        assert_eq!(latest_loaded.len(), 2);

        assert!(latest_loaded.contains(&(swap_id_1, state_3)));
        assert!(latest_loaded.contains(&(swap_id_2, state_4)));

        assert!(!latest_loaded.contains(&(swap_id_1, state_1)));
        assert!(!latest_loaded.contains(&(swap_id_1, state_2)));
    }

    #[tokio::test]
    async fn encrypted_database_migrates_plaintext_and_rejects_wrong_key() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("swap-db");
        File::create(&path)?;

        let swap_id = Uuid::new_v4();
        let state = State::Alice(AliceState::BtcRedeemed);

        let plaintext_db = SqliteDatabase::open(&path).await?;
        plaintext_db
            .insert_latest_state(swap_id, state.clone())
            .await?;
        plaintext_db.pool.close().await;

        let key = [1u8; 32];
        let encrypted_db =
            SqliteDatabase::open_with_cipher(&path, Some(StateCipher::from_key(key))).await?;
        assert_eq!(encrypted_db.get_state(swap_id).await?, state);

        let stored: String = sqlx::query_scalar("SELECT state FROM swap_states WHERE swap_id = ?")
            .bind(swap_id.to_string())
            .fetch_one(&encrypted_db.pool)
            .await?;
        assert!(stored.starts_with(STATE_CIPHER_PREFIX));
        assert_eq!(encrypted_db.all_with_dates().await?.len(), 1);
        encrypted_db.pool.close().await;

        let reopened =
            SqliteDatabase::open_with_cipher(&path, Some(StateCipher::from_key(key))).await?;
        assert_eq!(reopened.get_state(swap_id).await?, state);
        reopened.pool.close().await;

        let wrong =
            SqliteDatabase::open_with_cipher(&path, Some(StateCipher::from_key([2u8; 32]))).await;
        assert!(
            wrong.is_err(),
            "a different seed must not open the database"
        );

        Ok(())
    }

    #[tokio::test]
    async fn persisted_redeem_sweep_txid_survives_database_reopen() -> Result<()> {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("redeem-sweep.db");
        File::create(&path).unwrap();
        let swap_id = Uuid::new_v4();
        let first = TxHash("first-sweep-txid".to_owned());
        let second = TxHash("second-sweep-txid".to_owned());

        {
            let db = SqliteDatabase::open(&path).await?;
            assert_eq!(db.get_redeem_sweep_txid(swap_id).await?, None);
            db.insert_redeem_sweep_txid(swap_id, first.clone()).await?;
        }

        // Simulate the app/wallet-RPC restart: a new SqliteDatabase opens the
        // same file and must still find the txid persisted before the sweep
        // wait began.
        let db = SqliteDatabase::open(&path).await?;
        assert_eq!(db.get_redeem_sweep_txid(swap_id).await?, Some(first));
        db.insert_redeem_sweep_txid(swap_id, second.clone()).await?;
        assert_eq!(db.get_redeem_sweep_txid(swap_id).await?, Some(second));

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_load_monero_address() -> Result<()> {
        let db = setup_test_db().await?;

        let swap_id = Uuid::new_v4();
        let monero_address = "53gEuGZUhP9JMEBZoGaFNzhwEgiG7hwQdMCqFxiyiTeFPmkbt1mAoNybEUvYBKHcnrSgxnVWgZsTvRBaHBNXPa8tHiCU51a".parse()?;

        db.insert_monero_address(swap_id, monero_address).await?;

        let loaded_monero_address = db.get_monero_address(swap_id).await?;

        assert_eq!(monero_address, loaded_monero_address);

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_and_load_multiaddr() -> Result<()> {
        let db = setup_test_db().await?;

        let peer_id = PeerId::random();
        let multiaddr1 = "/ip4/127.0.0.1".parse::<Multiaddr>()?;
        let multiaddr2 = "/ip4/127.0.0.2".parse::<Multiaddr>()?;

        db.insert_address(peer_id, multiaddr1.clone()).await?;
        db.insert_address(peer_id, multiaddr2.clone()).await?;

        let loaded_multiaddr = db.get_addresses(peer_id).await?;

        assert!(loaded_multiaddr.contains(&multiaddr1));
        assert!(loaded_multiaddr.contains(&multiaddr2));
        assert_eq!(loaded_multiaddr.len(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_and_load_peer_id() -> Result<()> {
        let db = setup_test_db().await?;

        let peer_id = PeerId::random();
        let multiaddr1 = "/ip4/127.0.0.1".parse::<Multiaddr>()?;
        let multiaddr2 = "/ip4/127.0.0.2".parse::<Multiaddr>()?;

        db.insert_address(peer_id, multiaddr1.clone()).await?;
        db.insert_address(peer_id, multiaddr2.clone()).await?;

        let loaded_multiaddr = db.get_addresses(peer_id).await?;

        assert!(loaded_multiaddr.contains(&multiaddr1));
        assert!(loaded_multiaddr.contains(&multiaddr2));
        assert_eq!(loaded_multiaddr.len(), 2);

        Ok(())
    }

    #[tokio::test]
    async fn history_skips_corrupt_rows_instead_of_hiding_every_swap() -> Result<()> {
        let db = setup_test_db().await?;
        let good_id = Uuid::new_v4();
        db.insert_latest_state(good_id, State::Alice(AliceState::BtcRedeemed))
            .await?;

        let corrupt_id = Uuid::new_v4();
        sqlx::query("INSERT INTO swap_states (swap_id, entered_at, state) VALUES (?, ?, ?)")
            .bind(corrupt_id.to_string())
            .bind("2026-10-29 03:12:51.065863 +00:00:00")
            .bind("{not valid json")
            .execute(&db.pool)
            .await?;

        let history = db.all_with_dates().await?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].0, good_id);
        Ok(())
    }

    #[tokio::test]
    async fn protocol_v5_state_round_trips_and_encrypts() -> Result<()> {
        #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
        struct FakeState {
            step: u8,
            txid: String,
        }

        let dir = tempdir()?;
        let path = dir.path().join("protocol5.db");
        File::create(&path)?;
        let swap_id = Uuid::new_v4();
        let state = FakeState {
            step: 3,
            txid: "abc123".to_owned(),
        };

        {
            let db = SqliteDatabase::open(&path).await?;
            assert_eq!(db.load_protocol_v5_state::<FakeState>(swap_id, "bob").await?, None);
            db.store_protocol_v5_state(swap_id, "bob", &state).await?;
            assert_eq!(
                db.load_protocol_v5_state::<FakeState>(swap_id, "bob").await?,
                Some(FakeState { step: 3, txid: "abc123".to_owned() })
            );
            assert_eq!(db.load_protocol_v5_state::<FakeState>(swap_id, "alice").await?, None);
        }

        let key = [7u8; 32];
        let db = SqliteDatabase::open_with_cipher(&path, Some(StateCipher::from_key(key))).await?;
        assert_eq!(
            db.load_protocol_v5_state::<FakeState>(swap_id, "bob").await?,
            Some(FakeState { step: 3, txid: "abc123".to_owned() })
        );
        let all = db.all_protocol_v5_states("bob").await?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, swap_id);
        assert_eq!(all[0].1["step"], 3);
        assert!(db.all_protocol_v5_states("alice").await?.is_empty());

        let stored: String = sqlx::query_scalar(
            "SELECT state FROM protocol_v5_swap_states WHERE swap_id = ? AND role = ?",
        )
        .bind(swap_id.to_string())
        .bind("bob")
        .fetch_one(&db.pool)
        .await?;
        assert!(stored.starts_with(STATE_CIPHER_PREFIX));

        Ok(())
    }

    #[tokio::test]
    async fn protocol_v5_lock_inputs_conflict_and_release() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("protocol5-lock-inputs.db");
        File::create(&path)?;
        let db = SqliteDatabase::open(&path).await?;
        let outpoint = ::bitcoin::OutPoint::new("0000000000000000000000000000000000000000000000000000000000000000".parse::<::bitcoin::Txid>()?, 1);
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let swap_id = Uuid::new_v4();

        assert!(db
            .reserve_protocol_v5_lock_inputs(first, &[outpoint])
            .await?);
        assert!(!db
            .reserve_protocol_v5_lock_inputs(second, &[outpoint])
            .await?);
        db.rekey_protocol_v5_lock_inputs(first, swap_id).await?;
        assert!(!db
            .reserve_protocol_v5_lock_inputs(second, &[outpoint])
            .await?);
        db.release_protocol_v5_lock_inputs(swap_id).await?;
        assert!(db
            .reserve_protocol_v5_lock_inputs(second, &[outpoint])
            .await?);
        db.release_protocol_v5_lock_input_reservation(second)
            .await?;
        assert!(db
            .reserve_protocol_v5_lock_inputs(first, &[outpoint])
            .await?);

        Ok(())
    }

    #[tokio::test]
    async fn protocol_v5_open_clears_unbound_lock_input_reservations() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("protocol5-stale-reservation.db");
        File::create(&path)?;
        let outpoint = ::bitcoin::OutPoint::new(
            "1111111111111111111111111111111111111111111111111111111111111111"
                .parse::<::bitcoin::Txid>()?,
            0,
        );

        let db = SqliteDatabase::open(&path).await?;
        assert!(db
            .reserve_protocol_v5_lock_inputs(Uuid::new_v4(), &[outpoint])
            .await?);
        drop(db);

        let reopened = SqliteDatabase::open(&path).await?;
        assert!(
            reopened
                .reserve_protocol_v5_lock_inputs(Uuid::new_v4(), &[outpoint])
                .await?,
            "opening the database must clear pre-registration reservations left by a crash"
        );

        Ok(())
    }

    async fn setup_test_db() -> Result<SqliteDatabase> {
        let temp_db = tempdir().unwrap().into_path().join("tempdb");

        // file has to exist in order to connect with sqlite
        File::create(temp_db.clone()).unwrap();

        let db = SqliteDatabase::open(temp_db).await?;

        Ok(db)
    }
}
