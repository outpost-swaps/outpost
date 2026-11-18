package swap.model;

public enum SwapErrorType {
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
    // A failure after the Bitcoin lock may be published: resume, never end the swap (audit H-2).
    SwapInterrupted,

    UnknownError;

    public boolean shouldRestart() {
        return this == FailedToInitMoneroWallet || this == FailedToInitBitcoinWallet || this == DeserializeCborError || this == EncryptedSignatureTransferError || this == SellerEncounteredProblemError || this == EncryptedSignatureAckError || this == FailedToReceiveTransferProof || this == ElectrumMissingTransaction || this == ElectrumFailedToSubscribeToHeaders || this == RefundBroadcastFailed || this == SwapInterrupted;
    }
}
