use std::time::Duration;

use crate::{asb, bitcoin, cli};
use libp2p::request_response::{self, ProtocolSupport};
use libp2p::{PeerId, StreamProtocol};
use serde::{Deserialize, Serialize};

/// Aligned with upstream's `swap-p2p/src/protocols/quote.rs` (bid-quote/2.0.0):
/// JSON codec, mandatory refund policy, optional reserve proof. The live maker
/// fleet rejects bid-quote/1.0.0 (verified 2026-10-10).
const PROTOCOL: &str = "/comit/xmr/btc/bid-quote/2.0.0";
pub type OutEvent = request_response::Event<(), BidQuote>;
pub type Message = request_response::Message<(), BidQuote>;

pub type Behaviour = request_response::json::Behaviour<(), BidQuote>;

#[derive(Debug, Clone, Copy, Default)]
pub struct BidQuoteProtocol;

impl AsRef<str> for BidQuoteProtocol {
    fn as_ref(&self) -> &str {
        PROTOCOL
    }
}

/// The refund policy that will apply if the swap is cancelled, as advertised
/// in the quote. We currently always apply the full-refund policy.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(tag = "type", content = "content")]
pub enum RefundPolicyWire {
    /// Taker receives 100% of their Bitcoin back on refund.
    FullRefund,
    /// Upstream's anti-spam-deposit policy: part of the refund may be withheld
    /// by the maker. We do not apply this policy; accepted on the wire so we
    /// can parse quotes from makers that advertise it.
    PartialRefund {
        #[serde(deserialize_with = "deserialize_flexible_number")]
        anti_spam_deposit_ratio: serde_json::Number,
    },
}

/// Monero reserve proof attached to a quote by upstream v4 makers. We accept
/// and preserve it on the wire; parsing/verifying it is not implemented yet.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReserveProofWire {
    pub address: String,
    pub proof: String,
    pub message: String,
}

/// Represents a quote for buying XMR.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct BidQuote {
    /// The price at which the maker is willing to buy at.
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    pub price: bitcoin::Amount,
    /// The minimum quantity the maker is willing to buy.
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    pub min_quantity: bitcoin::Amount,
    /// The maximum quantity the maker is willing to buy.
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    pub max_quantity: bitcoin::Amount,
    /// The refund policy that will apply if the swap is cancelled.
    pub refund_policy: RefundPolicyWire,
    /// Optional Monero "ReserveProofV2" from the maker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_proof: Option<ReserveProofWire>,
}

/// Accepts a JSON number or numeric string for the ratio (rust_decimal on the
/// upstream side may serialize either way).
fn deserialize_flexible_number<'de, D>(deserializer: D) -> Result<serde_json::Number, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(number) => Ok(number),
        serde_json::Value::String(text) => {
            if let Ok(integer) = text.parse::<u64>() {
                return Ok(serde_json::Number::from(integer));
            }
            text.parse::<f64>()
                .map(serde_json::Number::from_f64)
                .map(|number| number.unwrap_or_else(|| serde_json::Number::from(0)))
                .map_err(|_| D::Error::custom("ratio is not a valid number"))
        }
        _ => Err(D::Error::custom("ratio must be a number")),
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("Received quote of 0")]
pub struct ZeroQuoteReceived;
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("Swap client disconnected")]
pub struct SwapDisconnected;
/// Constructs a new instance of the `quote` behaviour to be used by the ASB.
///
/// The ASB is always listening and only supports inbound connections, i.e.
/// handing out quotes.
pub fn asb() -> Behaviour {
    Behaviour::new(
        vec![(StreamProtocol::new(PROTOCOL), ProtocolSupport::Inbound)],
        request_response::Config::default().with_request_timeout(Duration::from_secs(60)),
    )
}

/// Constructs a new instance of the `quote` behaviour to be used by the CLI.
///
/// The CLI is always dialing and only supports outbound connections, i.e.
/// requesting quotes.
pub fn cli() -> Behaviour {
    Behaviour::new(
        vec![(StreamProtocol::new(PROTOCOL), ProtocolSupport::Outbound)],
        request_response::Config::default().with_request_timeout(Duration::from_secs(60)),
    )
}

impl From<(PeerId, Message)> for asb::OutEvent {
    fn from((peer, message): (PeerId, Message)) -> Self {
        match message {
            Message::Request { channel, .. } => Self::QuoteRequested { channel, peer },
            Message::Response { .. } => Self::unexpected_response(peer),
        }
    }
}
crate::impl_from_rr_event!(OutEvent, asb::OutEvent, PROTOCOL);

impl From<(PeerId, Message)> for cli::OutEvent {
    fn from((peer, message): (PeerId, Message)) -> Self {
        match message {
            Message::Request { .. } => Self::unexpected_request(peer),
            Message::Response {
                response,
                request_id,
            } => Self::QuoteReceived {
                id: request_id,
                response,
            },
        }
    }
}
crate::impl_from_rr_event!(OutEvent, cli::OutEvent, PROTOCOL);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_refund_quote() -> serde_json::Value {
        json!({
            "price": 103_908u64,
            "min_quantity": 10_000u64,
            "max_quantity": 200_000u64,
            "refund_policy": { "type": "FullRefund" }
        })
    }

    fn partial_refund_quote(ratio: serde_json::Value) -> serde_json::Value {
        json!({
            "price": 1000u64,
            "min_quantity": 100u64,
            "max_quantity": 200u64,
            "refund_policy": { "type": "PartialRefund", "content": { "anti_spam_deposit_ratio": ratio } }
        })
    }

    #[test]
    fn full_refund_quote_round_trips() {
        let quote: BidQuote = serde_json::from_value(full_refund_quote()).unwrap();
        assert_eq!(quote.price, bitcoin::Amount::from_sat(103_908));
        assert_eq!(quote.min_quantity, bitcoin::Amount::from_sat(10_000));
        assert_eq!(quote.max_quantity, bitcoin::Amount::from_sat(200_000));
        assert_eq!(quote.refund_policy, RefundPolicyWire::FullRefund);
        assert_eq!(quote.reserve_proof, None);

        let back: BidQuote =
            serde_json::from_value(serde_json::to_value(&quote).unwrap()).unwrap();
        assert_eq!(back, quote);
    }

    #[test]
    fn partial_refund_ratio_accepts_number_and_string() {
        for ratio in [json!(0.00005), json!("0.00005"), json!("1")] {
            let quote: BidQuote = serde_json::from_value(partial_refund_quote(ratio.clone()))
                .unwrap_or_else(|_| panic!("ratio {ratio} must parse"));
            match quote.refund_policy {
                RefundPolicyWire::PartialRefund {
                    anti_spam_deposit_ratio,
                } => assert!(
                    anti_spam_deposit_ratio
                        .to_string()
                        .parse::<f64>()
                        .is_ok(),
                    "parsed ratio must stay numeric"
                ),
                other => panic!("expected PartialRefund, got {other:?}"),
            }
        }
    }

    #[test]
    fn invalid_partial_refund_ratio_is_rejected() {
        for ratio in [json!("high"), json!({}), json!(null)] {
            assert!(
                serde_json::from_value::<BidQuote>(partial_refund_quote(ratio.clone())).is_err(),
                "ratio {ratio} must be rejected"
            );
        }
    }

    #[test]
    fn reserve_proof_is_optional_but_preserved() {
        let mut with_proof = full_refund_quote();
        with_proof["reserve_proof"] = json!({
            "address": "4AJe3UycSsPKJm9ZE6wNsc9iNEZRo7JBNX",
            "proof": "proof-bytes",
            "message": "message"
        });

        let quote: BidQuote = serde_json::from_value(with_proof).unwrap();
        let proof = quote
            .reserve_proof
            .as_ref()
            .expect("attached reserve proof must parse");
        assert_eq!(proof.proof, "proof-bytes");

        let reserialized = serde_json::to_value(&quote).unwrap();
        assert_eq!(reserialized["reserve_proof"]["proof"], "proof-bytes");

        let without_proof: BidQuote = serde_json::from_value(full_refund_quote()).unwrap();
        assert_eq!(without_proof.reserve_proof, None);
        assert!(serde_json::to_value(&without_proof)
            .unwrap()
            .get("reserve_proof")
            .is_none());
    }

    #[test]
    fn amounts_serialize_as_integer_satoshis() {
        let quote: BidQuote = serde_json::from_value(full_refund_quote()).unwrap();
        let serialized = serde_json::to_value(&quote).unwrap();
        assert_eq!(serialized["price"], json!(103_908u64));
        assert_eq!(serialized["min_quantity"], json!(10_000u64));
        assert_eq!(serialized["max_quantity"], json!(200_000u64));
    }
}
