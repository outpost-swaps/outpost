use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwapError {
    pub swap_id: String,
    pub error_type: ErrorType,
    pub error_message: String,
    pub fatal: bool
}

#[derive(Debug, Serialize, Deserialize)]
pub enum  ErrorType {
    SellerInsufficientBalance,
    SwapPunished,
    FailedToInitBitcoinWallet,
    FailedToInitRefundBitcoinWallet,
    FailedToInitMoneroWallet,
    EventLoopPanic,
    DeserializeCborError,
    EncryptedSignatureTransferError,
    EncryptedSignatureAckError,
    SellerEncounteredProblemError,
    FailedToInitAsbMoneroWallet,
    FailedToInitAsbBitcoinWallet,
    FailedToLoadSwapDatabase,
    FailedToLoadAsbDatabase,
    SwapDisconnected,
    FailedToReceiveTransferProof,
    ElectrumMissingTransaction,
    ElectrumFailedToSubscribeToHeaders,
    RefundBroadcastFailed,
    SwapSetupFailed,
    /// A failure after the Bitcoin lock may be published. The GUI resumes the swap: a stopped
    /// client cannot refund before the maker can punish (audit 2026-11-10, H-2).
    SwapInterrupted,
    UnknownError
}