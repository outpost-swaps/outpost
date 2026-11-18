package swap.helper;

import com.samourai.wallet.bip47.rpc.PaymentCode;
import java.util.Locale;
import java.util.Optional;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import org.bitcoinj.core.ECKey;
import org.bitcoinj.params.MainNetParams;
import org.json.JSONObject;
import org.json.JSONTokener;

/**
 * Checks a Dojo directory entry's signed block, as Ashigaru Desktop does
 * ({@code SignedMessageVerifier}), so a listing cannot be forged or altered in transit.
 *
 * <p>A block is a Bitcoin signed message whose text is the Dojo's pairing JSON followed by a line
 * {@code BIP47: PM8T...}, then the signature. An entry is accepted only if
 * <ol>
 *   <li>the key recovered from the signature has the same address as the notification address of the
 *       payment code embedded in the message (the operator really signed it), and
 *   <li>that is also the notification address of the payment code the directory advertises for the
 *       entry (a hostile directory cannot show an honest operator's name over its own signature).
 * </ol>
 * The URL and API key are then taken from the <em>signed</em> text, never from the unsigned fields
 * next to it. The signature proves who published the pairing, not that the Dojo is trustworthy.
 */
public final class HelperDojoSignedMessage {
    private static final Pattern PAYMENT_CODE = Pattern.compile("PM8T[1-9A-HJ-NP-Za-km-z]+");
    private static final String BEGIN_MESSAGE = "-----BEGIN BITCOIN SIGNED MESSAGE-----";
    private static final String BEGIN_SIGNATURE = "-----BEGIN BITCOIN SIGNATURE-----";
    private static final String END_SIGNATURE = "-----END BITCOIN SIGNATURE-----";

    private HelperDojoSignedMessage() {}

    /** What a verified block says: the operator's payment code and the pairing it published. */
    public record Signed(String paymentCode, String url, String apiKey) {}

    record Block(String message, String signature) {}

    /** The pairing from a block whose signature and payment-code binding check out; empty otherwise. Never throws. */
    public static Optional<Signed> verify(String signedBlock, String advertisedPaymentCode) {
        try {
            Block block = parse(signedBlock);
            PaymentCode embedded = embeddedPaymentCode(block.message());
            if (embedded == null) {
                return Optional.empty();
            }
            String notification = notificationAddress(embedded);
            String recovered = ECKey.signedMessageToKey(block.message(), block.signature()).toAddress(MainNetParams.get()).toString();
            if (!notification.equals(recovered)) {
                return Optional.empty();
            }
            if (advertisedPaymentCode != null && !advertisedPaymentCode.isBlank()
                    && !notification.equals(notificationAddress(new PaymentCode(advertisedPaymentCode.trim())))) {
                return Optional.empty();
            }
            JSONObject pairing = new JSONObject(new JSONTokener(block.message())).getJSONObject("pairing");
            String url = pairing.optString("url", "").trim();
            String key = pairing.optString("apikey", "").trim();
            if (url.isEmpty() || key.isEmpty()) {
                return Optional.empty();
            }
            return Optional.of(new Signed(embedded.toString(), url, key));
        } catch (Exception e) {
            return Optional.empty();
        }
    }

    private static String notificationAddress(PaymentCode code) throws Exception {
        return code.notificationAddress(MainNetParams.get()).getAddressString();
    }

    private static PaymentCode embeddedPaymentCode(String message) {
        Matcher matcher = PAYMENT_CODE.matcher(message);
        while (matcher.find()) {
            try {
                return new PaymentCode(matcher.group());
            } catch (Exception notACode) {
                // keep scanning
            }
        }
        return null;
    }

    static Block parse(String text) {
        if (text == null || text.isBlank()) {
            throw new IllegalArgumentException("empty block");
        }
        String upper = text.toUpperCase(Locale.ROOT);
        int messageStart = upper.indexOf(BEGIN_MESSAGE);
        int signatureStart = upper.indexOf(BEGIN_SIGNATURE);
        if (messageStart < 0 || signatureStart <= messageStart) {
            throw new IllegalArgumentException("missing markers");
        }
        int signatureEnd = upper.indexOf(END_SIGNATURE, signatureStart + BEGIN_SIGNATURE.length());
        if (signatureEnd < 0) {
            throw new IllegalArgumentException("missing end marker");
        }
        String message = text.substring(messageStart + BEGIN_MESSAGE.length(), signatureStart)
                .replaceFirst("^\\R+", "").replaceFirst("\\R+$", "");
        String signature = "";
        for (String line : text.substring(signatureStart + BEGIN_SIGNATURE.length(), signatureEnd).split("\\R")) {
            String trimmed = line.trim();
            String lower = trimmed.toLowerCase(Locale.ROOT);
            if (trimmed.isEmpty() || lower.startsWith("version:") || lower.startsWith("address:")) {
                continue;
            }
            signature = trimmed;
        }
        if (message.isEmpty() || signature.isEmpty()) {
            throw new IllegalArgumentException("no message or signature");
        }
        return new Block(message, signature);
    }
}
