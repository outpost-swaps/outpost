//! A counting stand-in for an Electrum server, for tests that assert how many requests a
//! sync makes. Generated from the `ElectrumApi` trait of `electrum-client` 0.24: every method
//! the sync does not use panics, so an unexpected request fails the test loudly.

#![allow(unused_variables)]

use bdk_electrum::electrum_client::bitcoin::{Script, Txid};
use bdk_electrum::electrum_client::*;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Default)]
pub struct CountingElectrum {
    /// Raw transactions the server can return, by txid.
    pub raw_txs: HashMap<Txid, Vec<u8>>,
    /// History per script (the script's bytes).
    pub history: HashMap<Vec<u8>, Vec<GetHistoryRes>>,
    /// How many `transaction_get_raw` requests were made.
    pub transaction_fetches: Arc<AtomicUsize>,
}

impl ElectrumApi for CountingElectrum {

    fn raw_call( &self, method_name: &str, params: impl IntoIterator<Item = Param>, ) -> Result<serde_json::Value, Error> { unimplemented!("the sync should not call raw_call") }

    fn batch_call(&self, batch: &Batch) -> Result<Vec<serde_json::Value>, Error> { unimplemented!("the sync should not call batch_call") }

    fn block_headers_subscribe_raw(&self) -> Result<RawHeaderNotification, Error> { unimplemented!("the sync should not call block_headers_subscribe_raw") }

    fn block_headers_pop_raw(&self) -> Result<Option<RawHeaderNotification>, Error> { unimplemented!("the sync should not call block_headers_pop_raw") }

    fn block_header_raw(&self, height: usize) -> Result<Vec<u8>, Error> { unimplemented!("the sync should not call block_header_raw") }

    fn block_headers(&self, start_height: usize, count: usize) -> Result<GetHeadersRes, Error> { unimplemented!("the sync should not call block_headers") }

    fn estimate_fee(&self, number: usize) -> Result<f64, Error> { unimplemented!("the sync should not call estimate_fee") }

    fn relay_fee(&self) -> Result<f64, Error> { unimplemented!("the sync should not call relay_fee") }

    fn script_subscribe(&self, script: &Script) -> Result<Option<ScriptStatus>, Error> { unimplemented!("the sync should not call script_subscribe") }

    fn batch_script_subscribe<'s, I>(&self, scripts: I) -> Result<Vec<Option<ScriptStatus>>, Error> where I: IntoIterator + Clone, I::Item: Borrow<&'s Script> { unimplemented!("the sync should not call batch_script_subscribe") }

    fn script_unsubscribe(&self, script: &Script) -> Result<bool, Error> { unimplemented!("the sync should not call script_unsubscribe") }

    fn script_pop(&self, script: &Script) -> Result<Option<ScriptStatus>, Error> { unimplemented!("the sync should not call script_pop") }

    fn script_get_balance(&self, script: &Script) -> Result<GetBalanceRes, Error> { unimplemented!("the sync should not call script_get_balance") }

    fn batch_script_get_balance<'s, I>(&self, scripts: I) -> Result<Vec<GetBalanceRes>, Error> where I: IntoIterator + Clone, I::Item: Borrow<&'s Script> { unimplemented!("the sync should not call batch_script_get_balance") }

    fn script_get_history(&self, script: &Script) -> Result<Vec<GetHistoryRes>, Error> { unimplemented!("the sync should not call script_get_history") }

    fn batch_script_get_history<'s, I>(&self, scripts: I) -> Result<Vec<Vec<GetHistoryRes>>, Error> where I: IntoIterator + Clone, I::Item: Borrow<&'s Script> {
        Ok(scripts
            .into_iter()
            .map(|script| {
                let script: &Script = script.borrow();
                self.history.get(script.as_bytes()).cloned().unwrap_or_default()
            })
            .collect())
    }

    fn script_list_unspent(&self, script: &Script) -> Result<Vec<ListUnspentRes>, Error> { unimplemented!("the sync should not call script_list_unspent") }

    fn batch_script_list_unspent<'s, I>( &self, scripts: I, ) -> Result<Vec<Vec<ListUnspentRes>>, Error> where I: IntoIterator + Clone, I::Item: Borrow<&'s Script> { unimplemented!("the sync should not call batch_script_list_unspent") }

    fn transaction_get_raw(&self, txid: &Txid) -> Result<Vec<u8>, Error> {
        self.transaction_fetches.fetch_add(1, Ordering::SeqCst);
        self.raw_txs
            .get(txid)
            .cloned()
            .ok_or_else(|| Error::Message(format!("unknown transaction {txid}")))
    }

    fn batch_transaction_get_raw<'t, I>(&self, txids: I) -> Result<Vec<Vec<u8>>, Error> where I: IntoIterator + Clone, I::Item: Borrow<&'t Txid> { unimplemented!("the sync should not call batch_transaction_get_raw") }

    fn batch_block_header_raw<I>(&self, heights: I) -> Result<Vec<Vec<u8>>, Error> where I: IntoIterator + Clone, I::Item: Borrow<u32> { unimplemented!("the sync should not call batch_block_header_raw") }

    fn batch_estimate_fee<I>(&self, numbers: I) -> Result<Vec<f64>, Error> where I: IntoIterator + Clone, I::Item: Borrow<usize> { unimplemented!("the sync should not call batch_estimate_fee") }

    fn transaction_broadcast_raw(&self, raw_tx: &[u8]) -> Result<Txid, Error> { unimplemented!("the sync should not call transaction_broadcast_raw") }

    fn transaction_get_merkle(&self, txid: &Txid, height: usize) -> Result<GetMerkleRes, Error> { unimplemented!("the sync should not call transaction_get_merkle") }

    fn batch_transaction_get_merkle<I>( &self, txids_and_heights: I, ) -> Result<Vec<GetMerkleRes>, Error> where I: IntoIterator + Clone, I::Item: Borrow<(Txid, usize)> { unimplemented!("the sync should not call batch_transaction_get_merkle") }

    fn txid_from_pos(&self, height: usize, tx_pos: usize) -> Result<Txid, Error> { unimplemented!("the sync should not call txid_from_pos") }

    fn txid_from_pos_with_merkle( &self, height: usize, tx_pos: usize, ) -> Result<TxidFromPosRes, Error> { unimplemented!("the sync should not call txid_from_pos_with_merkle") }

    fn server_features(&self) -> Result<ServerFeaturesRes, Error> { unimplemented!("the sync should not call server_features") }

    fn ping(&self) -> Result<(), Error> { Ok(()) }

}
