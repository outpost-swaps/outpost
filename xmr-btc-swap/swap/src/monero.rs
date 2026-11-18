pub mod wallet;
mod wallet_rpc;

pub mod distribution_cache;
pub mod ng;
pub mod witness;

pub use ::monero::network::Network;
pub use ::monero::{Address, PrivateKey, PublicKey};
pub use curve25519_dalek::scalar::Scalar;
pub use wallet::{redeem_restore_height, Wallet, REDEEM_RESTORE_HEIGHT_MARGIN};
pub use wallet_rpc::{WalletRpc, WalletRpcProcess};

use crate::bitcoin;
use anyhow::Result;
use rand::{CryptoRng, RngCore};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::fmt;
use std::ops::{Add, Mul, Sub};
use std::str::FromStr;

pub const PICONERO_OFFSET: u64 = 1_000_000_000_000;

#[derive(Serialize, Deserialize)]
#[serde(remote = "Network")]
#[allow(non_camel_case_types)]
pub enum network {
    Mainnet,
    Stagenet,
    Testnet,
}

pub fn private_key_from_secp256k1_scalar(scalar: bitcoin::Scalar) -> PrivateKey {
    let mut bytes = scalar.to_bytes();

    // we must reverse the bytes because a secp256k1 scalar is big endian, whereas a
    // ed25519 scalar is little endian
    bytes.reverse();

    PrivateKey::from_scalar(Scalar::from_bytes_mod_order(bytes))
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateViewKey(#[serde(with = "monero_private_key")] PrivateKey);

impl PrivateViewKey {
    pub fn new_random<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        let scalar = Scalar::random(rng);
        let private_key = PrivateKey::from_scalar(scalar);

        Self(private_key)
    }

    pub fn public(&self) -> PublicViewKey {
        PublicViewKey(PublicKey::from_private_key(&self.0))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn from_private_key(private_key: PrivateKey) -> Self {
        Self(private_key)
    }
}

impl Add for PrivateViewKey {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl From<PrivateViewKey> for PrivateKey {
    fn from(from: PrivateViewKey) -> Self {
        from.0
    }
}

impl From<PublicViewKey> for PublicKey {
    fn from(from: PublicViewKey) -> Self {
        from.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PublicViewKey(PublicKey);

#[derive(Debug, Copy, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd)]
pub struct Amount(u64);

// Conservative Monero fee reserve used when SIZING quotes and VALIDATING swap
// setup (it is not the fee actually paid for any transaction).
//
// Incident 2026-10-12 (swap 811069c3): the old reserve of 0.000016 XMR left a
// margin of only ~0.0000015 XMR above the quoted lock amount; when Bob's BTC
// lock confirmed, the wallet's real lock-transfer fee (ring size forced to 16)
// exceeded it and the transfer failed with RPC -17 "not enough money" — after
// the buyer's BTC was already locked. An ASB must refuse such swaps before the
// buyer commits on-chain, so the reserve must comfortably cover worst-case
// lock-transfer fees.
//
// Reference (eigenwallet core, swap-core/src/monero/primitives.rs): the
// highest average daily Monero fee in a year was ~0.00075 XMR; a 4x multiplier
// gives 0.003 XMR. Adopting the same conservative value.
pub const MONERO_FEE: Amount = Amount::from_piconero(3_000_000_000);

impl Amount {
    pub const ZERO: Self = Self(0);
    pub const ONE_XMR: Self = Self(PICONERO_OFFSET);
    /// Create an [Amount] with piconero precision and the given number of
    /// piconeros.
    ///
    /// A piconero (a.k.a atomic unit) is equal to 1e-12 XMR.
    pub const fn from_piconero(amount: u64) -> Self {
        Amount(amount)
    }

    /// Return Monero Amount as Piconero.
    pub fn as_piconero(&self) -> u64 {
        self.0
    }

    /// Calculate the maximum amount of Bitcoin that can be bought at a given
    /// asking price for this amount of Monero including the median fee.
    pub fn max_bitcoin_for_price(&self, ask_price: bitcoin::Amount) -> Option<bitcoin::Amount> {
        let pico_minus_fee = self.as_piconero().saturating_sub(MONERO_FEE.as_piconero());

        if pico_minus_fee == 0 {
            return Some(bitcoin::Amount::ZERO);
        }

        // safely convert the BTC/XMR rate to sat/pico
        let ask_sats = Decimal::from(ask_price.to_sat());
        let pico_per_xmr = Decimal::from(PICONERO_OFFSET);
        let ask_sats_per_pico = ask_sats / pico_per_xmr;

        let pico = Decimal::from(pico_minus_fee);
        let max_sats = pico.checked_mul(ask_sats_per_pico)?;
        let satoshi = max_sats.to_u64()?;

        Some(bitcoin::Amount::from_sat(satoshi))
    }

    pub fn from_monero(amount: f64) -> Result<Self> {
        let decimal = Decimal::try_from(amount)?;
        Self::from_decimal(decimal)
    }

    pub fn parse_monero(amount: &str) -> Result<Self> {
        let decimal = Decimal::from_str(amount)?;
        Self::from_decimal(decimal)
    }

    pub fn as_piconero_decimal(&self) -> Decimal {
        Decimal::from(self.as_piconero())
    }

    fn from_decimal(amount: Decimal) -> Result<Self> {
        let piconeros_dec =
            amount.mul(Decimal::from_u64(PICONERO_OFFSET).expect("constant to fit into u64"));
        let piconeros = piconeros_dec
            .to_u64()
            .ok_or_else(|| OverflowError(amount.to_string()))?;
        Ok(Amount(piconeros))
    }
}

impl Add for Amount {
    type Output = Amount;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl Sub for Amount {
    type Output = Amount;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

impl Mul<u64> for Amount {
    type Output = Amount;

    fn mul(self, rhs: u64) -> Self::Output {
        Self(self.0 * rhs)
    }
}

impl From<Amount> for u64 {
    fn from(from: Amount) -> u64 {
        from.0
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut decimal = Decimal::from(self.0);
        decimal
            .set_scale(12)
            .expect("12 is smaller than max precision of 28");
        write!(f, "{} XMR", decimal)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferProof {
    tx_hash: TxHash,
    #[serde(with = "monero_private_key")]
    tx_key: PrivateKey,
}

impl TransferProof {
    pub fn new(tx_hash: TxHash, tx_key: PrivateKey) -> Self {
        Self { tx_hash, tx_key }
    }
    pub fn tx_hash(&self) -> TxHash {
        self.tx_hash.clone()
    }
    pub fn tx_key(&self) -> PrivateKey {
        self.tx_key
    }
}

// TODO: add constructor/ change String to fixed length byte array
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxHash(pub String);

impl From<TxHash> for String {
    fn from(from: TxHash) -> Self {
        from.0
    }
}

impl fmt::Display for TxHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("expected {expected}, got {actual}")]
pub struct InsufficientFunds {
    pub expected: Amount,
    pub actual: Amount,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[error("Overflow, cannot convert {0} to u64")]
pub struct OverflowError(pub String);

pub mod monero_private_key {
    use monero::consensus::{Decodable, Encodable};
    use monero::PrivateKey;
    use serde::de::Visitor;
    use serde::ser::Error;
    use serde::{de, Deserializer, Serializer};
    use std::fmt;
    use std::io::Cursor;

    struct BytesVisitor;

    impl<'de> Visitor<'de> for BytesVisitor {
        type Value = PrivateKey;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "a byte array representing a Monero private key")
        }

        fn visit_bytes<E>(self, s: &[u8]) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let mut s = s;
            PrivateKey::consensus_decode(&mut s).map_err(|err| E::custom(format!("{:?}", err)))
        }

        fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let bytes = hex::decode(s).map_err(|err| E::custom(format!("{:?}", err)))?;
            PrivateKey::consensus_decode(&mut bytes.as_slice())
                .map_err(|err| E::custom(format!("{:?}", err)))
        }
    }

    pub fn serialize<S>(x: &PrivateKey, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut bytes = Cursor::new(vec![]);
        x.consensus_encode(&mut bytes)
            .map_err(|err| S::Error::custom(format!("{:?}", err)))?;
        if s.is_human_readable() {
            s.serialize_str(&hex::encode(bytes.into_inner()))
        } else {
            s.serialize_bytes(bytes.into_inner().as_ref())
        }
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<PrivateKey, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        let key = {
            if deserializer.is_human_readable() {
                deserializer.deserialize_string(BytesVisitor)?
            } else {
                deserializer.deserialize_bytes(BytesVisitor)?
            }
        };
        Ok(key)
    }
}

pub mod monero_amount {
    use crate::monero::Amount;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(x: &Amount, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_u64(x.as_piconero())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Amount, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        let picos = u64::deserialize(deserializer)?;
        let amount = Amount::from_piconero(picos);

        Ok(amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_monero_min() {
        let min_pics = 1;
        let amount = Amount::from_piconero(min_pics);
        let monero = amount.to_string();
        assert_eq!("0.000000000001 XMR", monero);
    }

    #[test]
    fn display_monero_one() {
        let min_pics = 1000000000000;
        let amount = Amount::from_piconero(min_pics);
        let monero = amount.to_string();
        assert_eq!("1.000000000000 XMR", monero);
    }

    #[test]
    fn display_monero_max() {
        let max_pics = 18_446_744_073_709_551_615;
        let amount = Amount::from_piconero(max_pics);
        let monero = amount.to_string();
        assert_eq!("18446744.073709551615 XMR", monero);
    }

    #[test]
    fn parse_monero_min() {
        let monero_min = "0.000000000001";
        let amount = Amount::parse_monero(monero_min).unwrap();
        let pics = amount.0;
        assert_eq!(1, pics);
    }

    #[test]
    fn parse_monero() {
        let monero = "123";
        let amount = Amount::parse_monero(monero).unwrap();
        let pics = amount.0;
        assert_eq!(123000000000000, pics);
    }

    #[test]
    fn parse_monero_max() {
        let monero = "18446744.073709551615";
        let amount = Amount::parse_monero(monero).unwrap();
        let pics = amount.0;
        assert_eq!(18446744073709551615, pics);
    }

    #[test]
    fn parse_monero_overflows() {
        let overflow_pics = "18446744.073709551616";
        let error = Amount::parse_monero(overflow_pics).unwrap_err();
        assert_eq!(
            error.downcast_ref::<OverflowError>().unwrap(),
            &OverflowError(overflow_pics.to_owned())
        );
    }

    #[test]
    fn max_bitcoin_to_trade() {
        // sanity check: if the asking price is 1 BTC / 1 XMR
        // and we have μ XMR + fee
        // then max BTC we can buy is μ
        let ask = bitcoin::Amount::from_btc(1.0).unwrap();

        let xmr = Amount::parse_monero("1.0").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_btc(1.0).unwrap());

        let xmr = Amount::parse_monero("0.5").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_btc(0.5).unwrap());

        let xmr = Amount::parse_monero("2.5").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_btc(2.5).unwrap());

        let xmr = Amount::parse_monero("420").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_btc(420.0).unwrap());

        let xmr = Amount::parse_monero("0.00001").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_btc(0.00001).unwrap());

        // other ask prices

        let ask = bitcoin::Amount::from_btc(0.5).unwrap();
        let xmr = Amount::parse_monero("2").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_btc(1.0).unwrap());

        let ask = bitcoin::Amount::from_btc(2.0).unwrap();
        let xmr = Amount::parse_monero("1").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_btc(2.0).unwrap());

        let ask = bitcoin::Amount::from_sat(382_900);
        let xmr = Amount::parse_monero("10").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_sat(3_829_000));

        // example from https://github.com/comit-network/xmr-btc-swap/issues/1084
        // with rate from kraken at that time
        let ask = bitcoin::Amount::from_sat(685_800);
        let xmr = Amount::parse_monero("0.826286435921").unwrap() + MONERO_FEE;
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(btc, bitcoin::Amount::from_sat(566_667));
    }

    #[test]
    fn max_bitcoin_to_trade_overflow() {
        let xmr = Amount::from_monero(30.0).unwrap() + MONERO_FEE;
        let ask = bitcoin::Amount::from_sat(728_688);
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(bitcoin::Amount::from_sat(21_860_640), btc);

        let xmr = Amount::from_piconero(u64::MAX);
        let ask = bitcoin::Amount::from_sat(u64::MAX);
        let btc = xmr.max_bitcoin_for_price(ask);

        assert!(btc.is_none());
    }

    #[test]
    fn geting_max_bitcoin_to_trade_with_balance_smaller_than_locking_fee() {
        let ask = bitcoin::Amount::from_sat(382_900);
        let xmr = Amount::parse_monero("0.00001").unwrap();
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(bitcoin::Amount::ZERO, btc);
    }

    /// Incident 2026-10-12 (swap 811069c3): a balance with only ~0.0028 XMR of
    /// headroom used to pass the 0.000016 XMR reserve and quoted a swap whose
    /// real lock-transfer fee exceeded the margin after Bob locked BTC.
    /// Anything below the conservative reserve must quote zero.
    #[test]
    fn balance_below_conservative_fee_reserve_quotes_zero() {
        let ask = bitcoin::Amount::from_sat(382_900);
        // 0.0029 XMR: above the old 0.000016 XMR reserve, below the new 0.003.
        let xmr = Amount::parse_monero("0.0029").unwrap();
        let btc = xmr.max_bitcoin_for_price(ask).unwrap();

        assert_eq!(bitcoin::Amount::ZERO, btc);
    }

    /// Same incident balance and ask price: the quote must still be tradeable
    /// but strictly smaller than the over-committed 143,758 sats the old
    /// reserve produced, and it must leave at least the fee reserve unspent.
    #[test]
    fn incident_balance_quotes_below_old_overcommitted_max() {
        let ask = bitcoin::Amount::from_sat(653_718);
        let balance = Amount::parse_monero("0.219925760000").unwrap();
        let btc = balance.max_bitcoin_for_price(ask).unwrap();

        assert!(btc > bitcoin::Amount::ZERO);
        assert!(btc.to_sat() < 143_758);

        // The remaining XMR after buying `btc` back at the ask covers the fee.
        let xmr_sold = btc.to_sat() * 1_000_000_000_000u64 / 653_718;
        assert!(balance.as_piconero() - xmr_sold >= MONERO_FEE.as_piconero());
    }

    use rand::rngs::OsRng;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct MoneroPrivateKey(#[serde(with = "monero_private_key")] crate::monero::PrivateKey);

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    pub struct MoneroAmount(#[serde(with = "monero_amount")] crate::monero::Amount);

    #[test]
    fn serde_monero_private_key_json() {
        let key = MoneroPrivateKey(monero::PrivateKey::from_scalar(
            crate::monero::Scalar::random(&mut OsRng),
        ));
        let encoded = serde_json::to_vec(&key).unwrap();
        let decoded: MoneroPrivateKey = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(key, decoded);
    }

    #[test]
    fn serde_monero_private_key_cbor() {
        let key = MoneroPrivateKey(monero::PrivateKey::from_scalar(
            crate::monero::Scalar::random(&mut OsRng),
        ));
        let encoded = serde_cbor::to_vec(&key).unwrap();
        let decoded: MoneroPrivateKey = serde_cbor::from_slice(&encoded).unwrap();
        assert_eq!(key, decoded);
    }

    #[test]
    fn serde_monero_amount() {
        let amount = MoneroAmount(crate::monero::Amount::from_piconero(1000));
        let encoded = serde_cbor::to_vec(&amount).unwrap();
        let decoded: MoneroAmount = serde_cbor::from_slice(&encoded).unwrap();
        assert_eq!(amount, decoded);
    }
}
