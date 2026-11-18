package swap.helper;

import com.samourai.wallet.crypto.AESUtil;
import com.samourai.wallet.util.CharSequenceX;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Base64;
import java.util.HexFormat;
import java.util.List;
import javax.crypto.Cipher;
import javax.crypto.spec.IvParameterSpec;
import javax.crypto.spec.SecretKeySpec;
import org.bitcoinj.crypto.MnemonicCode;
import org.bouncycastle.crypto.digests.SHA256Digest;
import org.bouncycastle.crypto.generators.PKCS5S2ParametersGenerator;
import org.bouncycastle.crypto.params.KeyParameter;
import org.json.JSONObject;
import org.json.JSONTokener;

/**
 * Reads an Ashigaru Wallet (Samourai lineage) encrypted backup file: the wallet seed, its BIP39
 * passphrase, the network and the Dojo pairing, in one step.
 *
 * <p>File layout (ashigaru-mobile {@code PayloadUtil}): a JSON object {@code {version, payload,
 * external}} that can be followed by stray bytes (the app rewrites the file without truncating it),
 * so only the first object is read. The payload is AES-256-CBC encrypted with the wallet's BIP39
 * passphrase: version 2 in the OpenSSL layout ({@code Salted__} + 8-byte salt, PBKDF2-HMAC-SHA256
 * with 15,000 rounds for key and IV), version 1 with the older SHA-1 scheme. The decrypted JSON has
 * {@code wallet.seed} (the BIP39 <em>entropy</em> as hex, so the words can be rebuilt),
 * {@code wallet.passphrase}, {@code wallet.testnet}, {@code wallet.fingerprint} and, when the wallet
 * has Dojo set, {@code meta.dojo.pairing}. The fingerprint in the result is the standard BIP32 form.
 *
 * <p>The fingerprint inside the backup is checked against the one derived from the seed and
 * passphrase, so a wrong passphrase or a damaged file is rejected instead of opening a different
 * wallet. Verified against a real backup (2026-11-09) and an independent implementation.
 */
public final class HelperAshigaruBackup {
    private static final int V2_ITERATIONS = 15000;
    private static final byte[] OPENSSL_MAGIC = "Salted__".getBytes(StandardCharsets.US_ASCII);

    private HelperAshigaruBackup() {}

    /** A backup that cannot be used; the message is for the user. */
    public static final class BackupException extends Exception {
        public BackupException(String message) {
            super(message);
        }
    }

    /** What the backup holds. {@code dojoUrl} and {@code dojoApiKey} are null when it has no Dojo. */
    public record Imported(
            String mnemonic,
            String passphrase,
            boolean testnet,
            String fingerprint,
            String dojoUrl,
            String dojoApiKey) {
        @Override
        public String toString() {
            // never print the words, the passphrase or the key
            return "Imported[testnet=" + testnet + ", fingerprint=" + fingerprint + ", dojo=" + (dojoUrl != null) + "]";
        }
    }

    public static Imported read(String fileText, String backupPassphrase) throws BackupException {
        if (fileText == null || backupPassphrase == null || backupPassphrase.isEmpty()) {
            throw new BackupException("Enter the passphrase of the wallet the backup came from.");
        }
        JSONObject wrapper;
        try {
            wrapper = new JSONObject(new JSONTokener(fileText.trim())); // first object only
        } catch (Exception e) {
            throw new BackupException("This is not an Ashigaru backup file.");
        }
        if (!wrapper.has("version") || !wrapper.has("payload")) {
            throw new BackupException("This is not an Ashigaru backup file.");
        }

        String decrypted = decrypt(wrapper.getInt("version"), wrapper.getString("payload"), backupPassphrase);
        JSONObject plain;
        try {
            plain = new JSONObject(decrypted);
        } catch (Exception e) {
            throw wrongPassphrase();
        }
        JSONObject wallet = plain.optJSONObject("wallet");
        if (wallet == null || !wallet.has("seed")) {
            throw new BackupException("The backup has no wallet seed in it.");
        }

        String mnemonic;
        try {
            mnemonic = mnemonicFromEntropyHex(wallet.getString("seed"));
        } catch (Exception e) {
            throw new BackupException("The seed in the backup is not valid.");
        }
        String bip39Passphrase = wallet.optString("passphrase", "");
        byte[] fingerprintBytes = HelperWalletImport.masterFingerprint(mnemonic, bip39Passphrase);
        String fingerprint = HexFormat.of().formatHex(fingerprintBytes);
        if (wallet.has("fingerprint") && !matchesBackupFingerprint(fingerprintBytes, wallet.getString("fingerprint"))) {
            throw wrongPassphrase();
        }

        String dojoUrl = null;
        String dojoApiKey = null;
        JSONObject meta = plain.optJSONObject("meta");
        JSONObject dojo = meta == null ? null : meta.optJSONObject("dojo");
        if (dojo != null) {
            JSONObject pairing = dojo.optJSONObject("pairing") != null ? dojo.optJSONObject("pairing") : dojo;
            dojoUrl = pairing.optString("url", null);
            dojoApiKey = pairing.optString("apikey", null);
        }
        return new Imported(mnemonic, bip39Passphrase, wallet.optBoolean("testnet", false), fingerprint, dojoUrl, dojoApiKey);
    }

    private static BackupException wrongPassphrase() {
        return new BackupException("Wrong passphrase, or the backup file is damaged.");
    }

    static String decrypt(int version, String payload, String passphrase) throws BackupException {
        try {
            if (version == 1) {
                return AESUtil.decrypt(payload, new CharSequenceX(passphrase), AESUtil.DefaultPBKDF2Iterations);
            }
            if (version == 2) {
                return decryptOpenSslSha256(payload, passphrase);
            }
        } catch (Exception e) {
            throw wrongPassphrase();
        }
        throw new BackupException("This backup was made by an Ashigaru version this app cannot read (backup version " + version + ").");
    }

    /** {@code openssl enc -aes-256-cbc -pbkdf2 -md sha256 -iter 15000} with the password as UTF-8. */
    static String decryptOpenSslSha256(String base64Payload, String passphrase) throws Exception {
        byte[] blob = Base64.getMimeDecoder().decode(base64Payload);
        if (blob.length < 32 || !Arrays.equals(Arrays.copyOfRange(blob, 0, 8), OPENSSL_MAGIC)) {
            throw new IllegalArgumentException("not an OpenSSL salted payload");
        }
        byte[] salt = Arrays.copyOfRange(blob, 8, 16);
        byte[] cipherText = Arrays.copyOfRange(blob, 16, blob.length);

        PKCS5S2ParametersGenerator kdf = new PKCS5S2ParametersGenerator(new SHA256Digest());
        kdf.init(passphrase.getBytes(StandardCharsets.UTF_8), salt, V2_ITERATIONS);
        byte[] keyAndIv = ((KeyParameter) kdf.generateDerivedParameters(48 * 8)).getKey();

        Cipher cipher = Cipher.getInstance("AES/CBC/PKCS5Padding");
        cipher.init(
                Cipher.DECRYPT_MODE,
                new SecretKeySpec(Arrays.copyOfRange(keyAndIv, 0, 32), "AES"),
                new IvParameterSpec(Arrays.copyOfRange(keyAndIv, 32, 48)));
        return new String(cipher.doFinal(cipherText), StandardCharsets.UTF_8);
    }

    /**
     * Writes a backup file that Ashigaru Mobile restores (Restore → backup file, then the passphrase).
     * Same layout Ashigaru writes ({@code PayloadUtil.getPayload} / {@code putPayload}): version 2,
     * {@code wallet.seed} = BIP39 entropy hex, {@code wallet.passphrase}, {@code wallet.testnet},
     * {@code wallet.fingerprint} in Ashigaru's byte-reversed form, and the Dojo pairing in
     * {@code meta.dojo}. Ashigaru's restore needs only the wallet fields; the rest is optional.
     * The payload is encrypted with the wallet's BIP39 passphrase, as Ashigaru does.
     */
    public static String write(String mnemonic, String passphrase, boolean testnet, HelperWalletImport.Dojo dojo) throws Exception {
        if (passphrase == null || passphrase.isEmpty()) {
            throw new IllegalArgumentException("Ashigaru only writes backups for wallets with a passphrase.");
        }
        List<String> words = Arrays.asList(HelperWalletImport.normalizeWords(mnemonic).split(" "));
        byte[] entropy = MnemonicCode.INSTANCE.toEntropy(words);
        byte[] fingerprint = HelperWalletImport.masterFingerprint(String.join(" ", words), passphrase);
        byte[] reversed = new byte[fingerprint.length];
        for (int i = 0; i < reversed.length; i++) {
            reversed[i] = fingerprint[reversed.length - 1 - i];
        }

        JSONObject wallet = new JSONObject()
                .put("testnet", testnet)
                .put("seed", HexFormat.of().formatHex(entropy))
                .put("passphrase", passphrase)
                .put("fingerprint", HexFormat.of().formatHex(reversed));
        Arrays.fill(entropy, (byte) 0);
        JSONObject meta = new JSONObject().put("version_name", "outpost");
        if (dojo != null) {
            meta.put("dojo", new JSONObject().put("pairing", new JSONObject()
                    .put("type", "dojo.api")
                    .put("version", "1.0.0")
                    .put("apikey", dojo.apiKey())
                    .put("url", dojo.url())));
        }
        String plain = new JSONObject().put("wallet", wallet).put("meta", meta).toString();
        return new JSONObject()
                .put("version", 2)
                .put("payload", encryptOpenSslSha256(plain, passphrase))
                .put("external", true)
                .toString();
    }

    /** {@code openssl enc -aes-256-cbc -pbkdf2 -md sha256 -iter 15000 -a}: base64 wrapped at 64 like Ashigaru's KOpenSSL. */
    static String encryptOpenSslSha256(String plain, String passphrase) throws Exception {
        byte[] salt = new byte[8];
        new java.security.SecureRandom().nextBytes(salt);
        PKCS5S2ParametersGenerator kdf = new PKCS5S2ParametersGenerator(new SHA256Digest());
        kdf.init(passphrase.getBytes(StandardCharsets.UTF_8), salt, V2_ITERATIONS);
        byte[] keyAndIv = ((KeyParameter) kdf.generateDerivedParameters(48 * 8)).getKey();

        Cipher cipher = Cipher.getInstance("AES/CBC/PKCS5Padding");
        cipher.init(
                Cipher.ENCRYPT_MODE,
                new SecretKeySpec(Arrays.copyOfRange(keyAndIv, 0, 32), "AES"),
                new IvParameterSpec(Arrays.copyOfRange(keyAndIv, 32, 48)));
        byte[] cipherText = cipher.doFinal(plain.getBytes(StandardCharsets.UTF_8));
        Arrays.fill(keyAndIv, (byte) 0);

        byte[] blob = new byte[16 + cipherText.length];
        System.arraycopy(OPENSSL_MAGIC, 0, blob, 0, 8);
        System.arraycopy(salt, 0, blob, 8, 8);
        System.arraycopy(cipherText, 0, blob, 16, cipherText.length);
        return Base64.getEncoder().encodeToString(blob).replaceAll("(.{64})", "$1\n");
    }

    /** The BIP39 words for the entropy hex in the backup ({@code wallet.seed}). */
    static String mnemonicFromEntropyHex(String hex) throws Exception {
        List<String> words = MnemonicCode.INSTANCE.toMnemonic(HexFormat.of().parseHex(hex));
        return String.join(" ", words);
    }

    /**
     * Ashigaru Wallet writes the fingerprint byte-reversed (bitcoinj's int, little-endian) while
     * Ashigaru Desktop, Sparrow and hardware wallets show the standard BIP32 form. Either is accepted.
     */
    static boolean matchesBackupFingerprint(byte[] standardFingerprint, String backupFingerprintHex) {
        byte[] reversed = new byte[standardFingerprint.length];
        for (int i = 0; i < reversed.length; i++) {
            reversed[i] = standardFingerprint[reversed.length - 1 - i];
        }
        HexFormat hex = HexFormat.of();
        return hex.formatHex(reversed).equalsIgnoreCase(backupFingerprintHex)
                || hex.formatHex(standardFingerprint).equalsIgnoreCase(backupFingerprintHex);
    }
}
