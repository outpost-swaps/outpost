use std::cmp::min;
use std::future::Future;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Result};
use bitcoin::Amount;
use jni::JNIEnv;
use jni::objects::{JObject, JString, JValue};
use jni::sys::{jstring};
use libp2p::Multiaddr;
use monero::Address;
use rust_decimal::prelude::ToPrimitive;
use url::Url;
use uuid::Uuid;
use crate::{fs, util};
use crate::asb::asb_data::AsbData;
use crate::asb::asb_xmr_balance_data::AsbXmrBalanceData;
use crate::monero::WalletRpc;
use crate::network::quote::{BidQuote, SwapDisconnected, ZeroQuoteReceived};
use crate::swap_error::SwapError;

macro_rules! jbytes_or_return {
    ($env:expr, $bytes:expr) => {
        match $env.byte_array_from_slice($bytes) {
            Ok(bytes) => JObject::from(bytes),
            Err(error) => {
                tracing::warn!("Failed to allocate a JNI byte array: {}", error);
                return;
            }
        }
    };
}

pub async fn determine_btc_to_swap<FB, TB, FMG, TMG, FS, TS, FFE, TFE>(
    swap_id: Uuid,
    bid_quote: impl Future<Output=Result<BidQuote>>,
    get_new_address: impl Future<Output=Result<bitcoin::Address>>,
    balance: FB,
    max_giveable_fn: FMG,
    sync: FS,
    estimate_fee: FFE,
    env: &JNIEnv<'_>,
) -> Result<(bitcoin::Amount, bitcoin::Amount)>
    where
        TB: Future<Output=Result<bitcoin::Amount>>,
        FB: Fn() -> TB,
        TMG: Future<Output=Result<bitcoin::Amount>>,
        FMG: Fn() -> TMG,
        TS: Future<Output=Result<()>>,
        FS: Fn() -> TS,
        FFE: Fn(bitcoin::Amount) -> TFE,
        TFE: Future<Output=Result<bitcoin::Amount>>,
{
    print_swap_log_ln(&env, format!("Requesting quote"));
    let bid_quote = bid_quote.await?;
    let maximum_amount = bid_quote.max_quantity;

    if bid_quote.max_quantity == bitcoin::Amount::ZERO {
        bail!(ZeroQuoteReceived)
    }

    print_swap_log_ln(&env, format!("Received quote: {}, {}, {}", bid_quote.price, bid_quote.min_quantity, bid_quote.max_quantity));

    let mut max_giveable = max_giveable_fn().await?;

    if max_giveable > maximum_amount {
        print_swap_log_ln(&env, format!("Amount to be locked with seller: {}", maximum_amount));
    } else {
        print_swap_log_ln(&env, format!("Amount to be locked with seller: {}", max_giveable));
    }

    if max_giveable == bitcoin::Amount::ZERO || max_giveable < bid_quote.min_quantity {
        let deposit_address = get_new_address.await?;
        let mut min_outstanding = util::dust_padded_outstanding(bid_quote.min_quantity, max_giveable);
        let mut min_fee = estimate_fee(min_outstanding).await?;
        let mut min_deposit = min_outstanding + min_fee;

        on_order_created(&env, swap_id.to_string(), deposit_address.to_string(), min_deposit, maximum_amount);

        loop {
            if get_running_swap(&env) {
                // Always sync and look for the deposit before asking Dojo for a
                // fresh fee estimate. Fee estimation can be slow or unavailable
                // and must not delay detecting an incoming mempool deposit.
                max_giveable = loop {
                    if !get_running_swap(&env) {
                        bail!(SwapDisconnected)
                    }
                    // While waiting for the deposit a transient Electrum/Tor
                    // failure must not abort the swap: keep syncing until the
                    // deposit appears or the user cancels.
                    if let Err(error) = sync().await {
                        tracing::warn!(
                            "Bitcoin wallet sync failed while waiting for the deposit: {error:#}"
                        );
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        continue;
                    }
                    match max_giveable_fn().await {
                        Ok(new_max_givable) if new_max_givable > max_giveable => {
                            break new_max_givable;
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            "Reading the Bitcoin wallet balance failed while waiting for the deposit: {error:#}"
                        ),
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                };

                let new_balance = match balance().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            "Reading the Bitcoin wallet balance failed while waiting for the deposit: {error:#}"
                        );
                        continue;
                    }
                };
                on_bitcoin_wallet_received(&env, new_balance, max_giveable, bid_quote.min_quantity);
                if max_giveable > maximum_amount {
                    print_swap_log_ln(&env, format!("New amount to be locked with seller: {}", maximum_amount));
                } else {
                    print_swap_log_ln(&env, format!("New amount to be locked with seller: {}", max_giveable));
                }

                if max_giveable < bid_quote.min_quantity {
                    continue;
                }

                // The deposit now covers the seller minimum. There is no
                // outstanding amount to fee-estimate; calling estimate_fee(0)
                // would fail the dust guard and strand the swap.
                break;
            } else {
                bail!(SwapDisconnected)
            }
        }
    };

    let balance = balance().await?;
    Ok(swap_amount_and_fee(
        max_giveable,
        balance,
        bid_quote.max_quantity,
    ))
}

/// Legacy buyer amount rule: spend all available funds up to the seller's
/// maximum. Only an amount above the seller maximum leaves change.
pub fn swap_amount_and_fee(
    max_giveable: Amount,
    balance: Amount,
    seller_max: Amount,
) -> (Amount, Amount) {
    let fees = Amount::from_sat(balance.to_sat().saturating_sub(max_giveable.to_sat()));
    (min(max_giveable, seller_max), fees)
}

#[cfg(test)]
mod amount_selection_tests {
    use super::*;

    #[test]
    fn below_seller_max_spends_the_whole_givable_balance() {
        let max_giveable = Amount::from_sat(99_846);
        let balance = Amount::from_sat(100_000);
        let seller_max = Amount::from_sat(500_000);

        let (amount, fees) = swap_amount_and_fee(max_giveable, balance, seller_max);

        assert_eq!(amount, max_giveable);
        assert_eq!(fees, Amount::from_sat(154));
        assert_eq!(amount + fees, balance, "no change should be created");
    }

    #[test]
    fn at_seller_max_spends_the_whole_givable_balance() {
        let max_giveable = Amount::from_sat(99_846);
        let balance = Amount::from_sat(100_000);
        let seller_max = Amount::from_sat(99_846);

        let (amount, fees) = swap_amount_and_fee(max_giveable, balance, seller_max);

        assert_eq!(amount, seller_max);
        assert_eq!(fees, Amount::from_sat(154));
        assert_eq!(amount + fees, balance);
    }

    #[test]
    fn deposit_above_min_quantity_needs_no_more_btc() {
        let min_quantity = Amount::from_sat(50_000);
        let max_giveable = Amount::from_sat(100_000);

        assert_eq!(
            dust_padded_outstanding(min_quantity, max_giveable),
            Amount::ZERO,
            "an already-funded deposit must not underflow or request more BTC"
        );
    }

    #[test]
    fn below_dust_deposit_is_padded_without_underflow() {
        let min_quantity = Amount::from_sat(100_000);
        let max_giveable = Amount::from_sat(99_000);

        assert_eq!(
            dust_padded_outstanding(min_quantity, max_giveable),
            Amount::from_sat(3_000)
        );
    }

    #[test]
    fn above_seller_max_caps_the_amount_and_leaves_change() {
        let max_giveable = Amount::from_sat(99_846);
        let balance = Amount::from_sat(100_000);
        let seller_max = Amount::from_sat(50_000);

        let (amount, fees) = swap_amount_and_fee(max_giveable, balance, seller_max);

        assert_eq!(amount, seller_max);
        assert_eq!(fees, Amount::from_sat(154));
        assert_eq!(balance - amount - fees, Amount::from_sat(49_846));
    }
}

/// Pure deposit-wait math shared by `determine_btc_to_swap`: how many BTC must
/// still arrive for `min_quantity` to be lockable, padded by the relay-dust
/// limit so the wallet's fee-rate estimate doesn't fail on a sub-dust
/// outstanding amount (e.g. a user sending slightly too little).
pub fn dust_padded_outstanding(min_quantity: Amount, max_giveable: Amount) -> Amount {
    let dust = Amount::from_sat(2000);
    // max_giveable can exceed min_quantity after a deposit arrives. Subtraction
    // must not underflow (Amount's Sub panics); an already-funded swap needs no
    // additional deposit, so return zero instead of padding a negative amount.
    let min_outstanding =
        Amount::from_sat(min_quantity.to_sat().saturating_sub(max_giveable.to_sat()));
    if min_outstanding > Amount::ZERO && min_outstanding < dust {
        min_outstanding + dust
    } else {
        min_outstanding
    }
}

/// Install the process-wide rustls provider once. Both `ring` and
/// `aws-lc-rs` can be enabled by the dependency graph, so rustls 0.23 refuses
/// to choose one automatically when a TLS client is first created.
pub fn install_default_crypto_provider() {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
}

pub fn on_xmr_lock_confirmation(env: &JNIEnv, txid: String, confirmations: u64) {
    let listener = get_swap_listener(&env);
    let txid_bytes = jbytes_or_return!(env, txid.as_bytes());
    let confs = JValue::from(confirmations.to_i64().unwrap_or(0));
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onXmrLockConfirmation", "([BJ)V", &[JValue::from(txid_bytes), confs]);
    }
}

pub fn on_asb_xmr_balance_data(env: &JNIEnv, data: AsbXmrBalanceData) {
    let listener = get_asb_listener(&env);
    let Ok(asb_balance_data_json) = serde_json::to_string(&data) else {
        tracing::warn!("Failed to serialize ASB balance data");
        return;
    };
    let balance_json_bytes = jbytes_or_return!(env, asb_balance_data_json.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onAsbXmrBalanceData", "([B)V", &[JValue::from(balance_json_bytes)]);
    }
}

pub fn on_xmr_rpc_download_progress(env: &JNIEnv, pct: u64) {
    let listener = get_rpc_download_listener(&env);
    let percent = JValue::from(pct.to_i64().unwrap_or(0));
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onXmrRpcDownloadProgress", "(J)V", &[percent]);
    }
}

pub fn on_order_created(env: &JNIEnv, swap_id: String, address: String, min_quantity: Amount, max_quantity: Amount) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let address_bytes = jbytes_or_return!(env, address.as_bytes());
    let min_sats = JValue::from(min_quantity.to_sat().to_i64().unwrap_or(0));
    let max_sats = JValue::from(max_quantity.to_sat().to_i64().unwrap_or(0));
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onOrderCreated", "([B[BJJ)V", &[JValue::from(swap_id_bytes), JValue::from(address_bytes), min_sats, max_sats]);
    }
}

pub fn on_swap_completed(env: &JNIEnv, swap_id: String) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onSwapCompleted", "([B)V", &[JValue::from(swap_id_bytes)]);
    }
}

pub fn on_swap_running(env: &JNIEnv, swap_id: String, multiaddr: Multiaddr) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let multiaddr_bytes = jbytes_or_return!(env, multiaddr.to_string().as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onSwapRunning", "([B[B)V", &[JValue::from(swap_id_bytes), JValue::from(multiaddr_bytes)]);
    }
}

pub fn on_swap_resume_context(
    env: &JNIEnv,
    swap_id: String,
    seller_multiaddr: Multiaddr,
    monero_address: Address,
    btc_sats: u64,
    xmr_piconero: u64,
) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let multiaddr_bytes = jbytes_or_return!(env, seller_multiaddr.to_string().as_bytes());
    let address_bytes = jbytes_or_return!(env, monero_address.to_string().as_bytes());
    let btc = JValue::from(btc_sats.to_i64().unwrap_or(0));
    let xmr = JValue::from(xmr_piconero.to_i64().unwrap_or(0));
    if let JValue::Object(listener) = listener {
        let result = env.call_method(
            listener,
            "onSwapResumeContext",
            "([B[B[BJJ)V",
            &[JValue::from(swap_id_bytes), JValue::from(multiaddr_bytes), JValue::from(address_bytes), btc, xmr],
        );
    }
}

/// The seller kept the anti-spam deposit after a partial refund: the swap ended with
/// `refunded_sats` (before fees) back and `deposit_sats` lost.
pub fn on_swap_partially_refunded(env: &JNIEnv, swap_id: String, refunded_sats: u64, deposit_sats: u64) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let refunded = JValue::from(refunded_sats.to_i64().unwrap_or(0));
    let deposit = JValue::from(deposit_sats.to_i64().unwrap_or(0));
    if let JValue::Object(listener) = listener {
        let _ = env.call_method(
            listener,
            "onSwapPartiallyRefunded",
            "([BJJ)V",
            &[JValue::from(swap_id_bytes), refunded, deposit],
        );
    }
}

pub fn on_generic_seller_quote_error(env: &JNIEnv, swap_id: String, error: String) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let error_bytes = jbytes_or_return!(env, error.to_string().as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onGenericSellerQuoteError", "([B[B)V", &[JValue::from(swap_id_bytes), JValue::from(error_bytes)]);
    }
}

pub fn on_asb_initialized(env: &JNIEnv, asb_data: AsbData) {
    let Ok(asb_data_json_string) = serde_json::to_string(&asb_data) else {
        tracing::warn!("Failed to serialize ASB data");
        return;
    };
    let asb_data_json_bytes = jbytes_or_return!(env, asb_data_json_string.as_bytes());
    let listener = get_asb_listener(&env);
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onAsbInitialized", "([B)V", &[JValue::from(asb_data_json_bytes)]);
    }
}

pub const ON_SWAP_SAFELY_ABORTED_METHOD: &str = "onSwapSafelyAborted";
pub const ON_SWAP_REFUNDED_METHOD: &str = "onSwapRefunded";
pub const ON_SWAP_CANCEL_PREPARED_METHOD: &str = "onSwapCancelPrepared";
pub const ON_BTC_REDEEMED_METHOD: &str = "onBtcRedeemed";
pub const ON_START_REDEEM_XMR_SYNC_METHOD: &str = "onStartRedeemXmrSync";
pub const ON_START_XMR_SWEEP_METHOD: &str = "onStartXmrSweep";

pub fn on_swap_error(env: &JNIEnv, error: SwapError) {
    let Ok(fatal_error_json) = serde_json::to_string(&error) else {
        tracing::warn!("Failed to serialize swap error");
        return;
    };
    call_basic_listener_method(&env, fatal_error_json, "onSwapError");
}

pub fn on_asb_error(env: &JNIEnv, error: SwapError) {
    let Ok(fatal_error_json) = serde_json::to_string(&error) else {
        tracing::warn!("Failed to serialize ASB error");
        return;
    };
    call_basic_asb_listener_method(&env, fatal_error_json, "onAsbError");
}

pub fn call_basic_listener_method(env: &JNIEnv, message: String, method: &str) {
    let listener = get_swap_listener(&env);
    let message_bytes = jbytes_or_return!(env, message.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, method, "([B)V", &[JValue::from(message_bytes)]);
    }
}

pub fn call_basic_asb_listener_method(env: &JNIEnv, message: String, method: &str) {
    let listener = get_asb_listener(&env);
    let message_bytes = jbytes_or_return!(env, message.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, method, "([B)V", &[JValue::from(message_bytes)]);
    }
}

pub fn on_swap_canceled(env: &JNIEnv, swap_id: String, btc_cancel_txid: String) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let btc_cancel_txid_bytes = jbytes_or_return!(env, btc_cancel_txid.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onSwapCanceled", "([B[B)V", &[JValue::from(swap_id_bytes), JValue::from(btc_cancel_txid_bytes)]);
    }
}

pub fn on_swap_refund_broadcast(env: &JNIEnv, swap_id: String, btc_refund_txid: String) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let btc_refund_txid_bytes = jbytes_or_return!(env, btc_refund_txid.as_bytes());
    if let JValue::Object(listener) = listener {
        let _ = env.call_method(listener, "onSwapRefundBroadcast", "([B[B)V", &[JValue::from(swap_id_bytes), JValue::from(btc_refund_txid_bytes)]);
    }
}

pub fn on_swap_amount_determined(env: &JNIEnv, amount: Amount) {
    let listener = get_swap_listener(&env);
    let sats = JValue::from(amount.to_sat().to_i64().unwrap_or(0));
    if let JValue::Object(listener) = listener {
        let _ = env.call_method(listener, "onSwapAmountDetermined", "(J)V", &[sats]);
    }
}

pub fn on_bitcoin_wallet_received(env: &JNIEnv, new_balance: Amount, max_giveable: Amount, min_quanity: Amount) {
    let listener = get_swap_listener(&env);
    let balance_satoshis = JValue::from(new_balance.to_sat().to_i64().unwrap_or(0));
    let max_giveable_satoshis = JValue::from(max_giveable.to_sat().to_i64().unwrap_or(0));
    let min_quanity_satoshis = JValue::from(min_quanity.to_sat().to_i64().unwrap_or(0));
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onBtcReceived", "(JJJ)V", &[balance_satoshis, max_giveable_satoshis, min_quanity_satoshis]);
    }
}

pub fn on_xmr_lock_proof_received(env: &JNIEnv, swap_id: String, xmr_lock_txid: String) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let xmr_lock_txid_bytes = jbytes_or_return!(env, xmr_lock_txid.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onXmrLockProofReceived", "([B[B)V", &[JValue::from(swap_id_bytes), JValue::from(xmr_lock_txid_bytes)]);
    }
}

/// The Monero sweep reached the daemon; only a block is missing.
pub fn on_xmr_sweep_broadcast(env: &JNIEnv, swap_id: String, sweep_txid: String) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    let sweep_txid_bytes = jbytes_or_return!(env, sweep_txid.as_bytes());
    if let JValue::Object(listener) = listener {
        let _ = env.call_method(listener, "onXmrSweepBroadcast", "([B[B)V", &[JValue::from(swap_id_bytes), JValue::from(sweep_txid_bytes)]);
    }
}

pub const ON_XMR_RECEIVED_METHOD: &str = "onXmrReceived";
pub const ON_XMR_RECEIVED_SIGNATURE: &str = "([BJJ)V";

/// The exact XMR the sweep pays the buyer and its network fee, in piconero (matrix N-15).
pub fn on_xmr_received(env: &JNIEnv, swap_id: String, received_piconero: u64, fee_piconero: u64) {
    let listener = get_swap_listener(&env);
    let swap_id_bytes = jbytes_or_return!(env, swap_id.as_bytes());
    // Java's long is signed; a piconero amount stays far below i64::MAX (9.2 million XMR).
    let (Ok(received), Ok(fee)) = (i64::try_from(received_piconero), i64::try_from(fee_piconero)) else {
        return;
    };
    if let JValue::Object(listener) = listener {
        let _ = env.call_method(
            listener,
            ON_XMR_RECEIVED_METHOD,
            ON_XMR_RECEIVED_SIGNATURE,
            &[JValue::from(swap_id_bytes), JValue::Long(received), JValue::Long(fee)],
        );
    }
}

pub fn on_btc_locked(env: &JNIEnv, btc_lock_txid: String) {
    let listener = get_swap_listener(&env);
    let btc_lock_txid_bytes = jbytes_or_return!(env, btc_lock_txid.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onBtcLocked", "([B)V", &[JValue::from(btc_lock_txid_bytes)]);
    }
}

pub fn on_btc_lock_tx_confirm(env: &JNIEnv, btc_lock_txid: String) {
    let listener = get_swap_listener(&env);
    let btc_lock_txid_bytes = jbytes_or_return!(env, btc_lock_txid.as_bytes());
    if let JValue::Object(listener) = listener {
        let result = env.call_method(listener, "onBtcLockConfirm", "([B)V", &[JValue::from(btc_lock_txid_bytes)]);
    }
}

pub fn print_swap_log_ln(env: &JNIEnv, message: String) {
    let listener = get_swap_listener(env);
    let Ok(message_bytes) = env.byte_array_from_slice(message.as_bytes()) else {
        return;
    };
    if let JValue::Object(listener) = listener {
        let _ = env.call_method(
            listener,
            "printSwapLogLn",
            "([B)V",
            &[JValue::from(JObject::from(message_bytes))],
        );
    }
}

pub fn get_swap_listener<'a>(env: &'a JNIEnv<'a>) -> JValue<'a> {
    listener_or_void(
        env,
        "swap/gui/controller/MainController",
        "swapListener",
        "Lswap/listener/SwapListener;",
    )
}

pub fn get_asb_listener<'a>(env: &'a JNIEnv<'a>) -> JValue<'a> {
    listener_or_void(
        env,
        "swap/gui/controller/MainController",
        "asbListener",
        "Lswap/listener/AsbListener;",
    )
}

pub fn get_rpc_download_listener<'a>(env: &'a JNIEnv<'a>) -> JValue<'a> {
    listener_or_void(
        env,
        "swap/gui/controller/PairingController",
        "rpcDownloadListener",
        "Lswap/listener/RpcDownloadListener;",
    )
}

fn listener_or_void<'a>(
    env: &'a JNIEnv<'a>,
    class: &str,
    field: &str,
    signature: &str,
) -> JValue<'a> {
    env.find_class(class)
        .ok()
        .and_then(|controller| env.get_static_field(controller, field, signature).ok())
        .unwrap_or(JValue::Void)
}

pub fn get_string_value(env: &JNIEnv, java_string: jstring) -> Result<String> {
    Ok(env.get_string(JString::from(java_string))?.into())
}

pub fn jstring_to_xmr_address(env: &JNIEnv, address_jstring: jstring) -> Result<Address> {
    let string_value = get_string_value(env, address_jstring)?;
    Ok(Address::from_str(string_value.as_str())?)
}

pub fn jstring_to_libp2p_multiaddr(
    env: &JNIEnv,
    multiaddr_jstring: jstring,
) -> Result<Multiaddr> {
    let string_value = get_string_value(env, multiaddr_jstring)?;
    Ok(Multiaddr::from_str(string_value.as_str())?)
}

pub fn jstring_to_url(env: &JNIEnv, url_jstring: jstring) -> Result<Url> {
    let string_value = get_string_value(env, url_jstring)?;
    Ok(Url::from_str(string_value.as_str())?)
}

pub async fn maybe_download_xmr_rpc(
    env: &JNIEnv<'_>,
    data_dir: PathBuf,
    proxy_string: String,
) -> Result<WalletRpc> {
    let _monero_wallet_rpc = WalletRpc::new(Option::Some(&env), data_dir.join("monero"), proxy_string).await;
    _monero_wallet_rpc
}

pub fn get_sys_time_in_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// Resolves once the swap has been stopped (the user pressed Disconnect), checking every `poll`.
/// Race it against a long setup step so a Disconnect does not wait for the step to finish
/// (2026-11-14: a Disconnect during "Preparing swap" took effect only after the 170 s Bitcoin sync).
pub async fn wait_until_stopped(is_running: impl Fn() -> bool, poll: std::time::Duration) {
    while is_running() {
        tokio::time::sleep(poll).await;
    }
}

pub fn get_running_swap(env: &JNIEnv) -> bool {
    let running = std::cell::Cell::new(false);
    let _ = env.with_local_frame(8, || {
        let listener = get_swap_listener(env);
        if let JValue::Object(listener) = listener {
            running.set(
                env.call_method(listener, "getRunningSwap", "()Z", &[])
                    .ok()
                    .and_then(|result| result.z().ok())
                    .unwrap_or(false),
            );
        }
        Ok(JObject::null())
    });
    running.get()
}

pub fn get_running_asb(env: &JNIEnv) -> bool {
    let running = std::cell::Cell::new(false);
    let _ = env.with_local_frame(8, || {
        let listener = get_asb_listener(env);
        if let JValue::Object(listener) = listener {
            running.set(
                env.call_method(listener, "getRunningAsb", "()Z", &[])
                    .ok()
                    .and_then(|result| result.z().ok())
                    .unwrap_or(false),
            );
        }
        Ok(JObject::null())
    });
    running.get()
}
#[cfg(test)]
mod tests {
    #[test]
    fn tls_provider_is_available_for_client_config() {
        super::install_default_crypto_provider();
        let _ = tokio_rustls::rustls::ClientConfig::builder();
    }

    use super::*;

    #[test]
    fn outstanding_spans_full_gap_without_padding() {
        assert_eq!(
            dust_padded_outstanding(Amount::from_sat(100_000), Amount::ZERO),
            Amount::from_sat(100_000)
        );
    }

    #[test]
    fn outstanding_accounts_for_already_giveable_amount() {
        assert_eq!(
            dust_padded_outstanding(Amount::from_sat(100_000), Amount::from_sat(40_000)),
            Amount::from_sat(60_000)
        );
    }

    #[test]
    fn sub_dust_outstanding_is_padded_for_fee_estimation() {
        assert_eq!(
            dust_padded_outstanding(Amount::from_sat(1_500), Amount::ZERO),
            Amount::from_sat(3_500)
        );
    }

    #[test]
    fn outstanding_exactly_at_dust_limit_is_not_padded() {
        assert_eq!(
            dust_padded_outstanding(Amount::from_sat(2_000), Amount::ZERO),
            Amount::from_sat(2_000)
        );
    }
}

#[cfg(test)]
mod wait_until_stopped_tests {
    use super::wait_until_stopped;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn resolves_soon_after_the_swap_is_stopped() {
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            flag.store(false, Ordering::SeqCst);
        });
        let started = tokio::time::Instant::now();
        wait_until_stopped(|| running.load(Ordering::SeqCst), Duration::from_millis(250)).await;
        let waited = started.elapsed();
        stopper.await.unwrap();
        assert!(waited >= Duration::from_secs(5), "{waited:?}");
        assert!(waited < Duration::from_millis(5_500), "{waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn returns_at_once_when_already_stopped() {
        let started = tokio::time::Instant::now();
        wait_until_stopped(|| false, Duration::from_millis(250)).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_longer_step_loses_the_race_to_a_disconnect() {
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            flag.store(false, Ordering::SeqCst);
        });
        let outcome = tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(170)) => "setup finished",
            _ = wait_until_stopped(|| running.load(Ordering::SeqCst), Duration::from_millis(250)) => "stopped",
        };
        assert_eq!(outcome, "stopped");
    }
}
