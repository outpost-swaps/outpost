package swap.helper;

import com.samourai.wallet.crypto.AESUtil;
import com.samourai.wallet.segwit.SegwitAddress;
import com.samourai.wallet.util.CharSequenceX;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;
import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.crypto.ChildNumber;
import org.bitcoinj.crypto.DeterministicKey;
import org.bitcoinj.crypto.HDKeyDerivation;
import org.bitcoinj.crypto.MnemonicCode;
import org.bitcoinj.crypto.MnemonicException;
import org.bitcoinj.params.MainNetParams;
import org.bitcoinj.params.TestNet3Params;
import org.json.JSONObject;

/**
 * The checks and conversions behind "Import wallet" on the pairing screen, free of any window so
 * they can be tested: BIP39 words, the fingerprint and first receive address a user compares with
 * their wallet, Dojo input, and the pairing record the app stores ({@code HelperProperties.init}).
 */
public final class HelperWalletImport {
    private HelperWalletImport() {}

    /** What a user compares with Ashigaru before trusting an import. */
    public record Verification(byte[] fingerprint, String fingerprintHex, String firstReceiveAddress) {}

    /** A Dojo the wallet talks to. */
    public record Dojo(String url, String apiKey) {}

    /** Lower-cases the words and collapses any whitespace (pasted words often carry line breaks). */
    public static String normalizeWords(String text) {
        return text == null ? "" : text.trim().toLowerCase(Locale.ROOT).replaceAll("\\s+", " ");
    }

    /** {@code null} when the words are a valid BIP39 mnemonic, otherwise a message for the user. */
    public static String checkWords(String normalizedWords) {
        if (normalizedWords == null || normalizedWords.isBlank()) {
            return "Enter the 12 or 24 words of the wallet.";
        }
        List<String> words = Arrays.asList(normalizedWords.split(" "));
        if (words.size() != 12 && words.size() != 15 && words.size() != 18 && words.size() != 21 && words.size() != 24) {
            return "A wallet has 12 or 24 words; this has " + words.size() + ".";
        }
        try {
            MnemonicCode.INSTANCE.check(words);
            return null;
        } catch (MnemonicException.MnemonicWordException e) {
            return "\"" + e.badWord + "\" is not a valid word.";
        } catch (MnemonicException e) {
            return "The words do not add up (the last word is a checksum): check for a typo or a missing word.";
        }
    }

    /** The standard BIP32 master fingerprint: the first 4 bytes of HASH160 of the master public key. */
    public static byte[] masterFingerprint(String mnemonic, String bip39Passphrase) {
        byte[] seed = MnemonicCode.toSeed(Arrays.asList(mnemonic.split("\\s+")), bip39Passphrase == null ? "" : bip39Passphrase);
        DeterministicKey master = HDKeyDerivation.createMasterPrivateKey(seed);
        return ByteBuffer.allocate(4).putInt(master.getFingerprint()).array();
    }

    /** The first native-segwit receive address, {@code m/84'/coin'/0'/0/0}: what the wallet shows first. */
    public static String firstReceiveAddress(String mnemonic, String bip39Passphrase, boolean testnet) {
        byte[] seed = MnemonicCode.toSeed(Arrays.asList(mnemonic.split("\\s+")), bip39Passphrase == null ? "" : bip39Passphrase);
        DeterministicKey key = HDKeyDerivation.createMasterPrivateKey(seed);
        int[] path = {84 | ChildNumber.HARDENED_BIT, (testnet ? 1 : 0) | ChildNumber.HARDENED_BIT, ChildNumber.HARDENED_BIT, 0, 0};
        for (int child : path) {
            key = HDKeyDerivation.deriveChildKey(key, child);
        }
        NetworkParameters params = testnet ? TestNet3Params.get() : MainNetParams.get();
        return new SegwitAddress(key.getPubKey(), params).getBech32AsString();
    }

    public static Verification verify(String mnemonic, String bip39Passphrase, boolean testnet) {
        byte[] fingerprint = masterFingerprint(mnemonic, bip39Passphrase);
        return new Verification(
                fingerprint,
                java.util.HexFormat.of().formatHex(fingerprint),
                firstReceiveAddress(mnemonic, bip39Passphrase, testnet));
    }

    /**
     * Dojo from pasted JSON or from separate fields. Accepts the Dojo's own pairing JSON
     * ({@code {"pairing":{"url","apikey",...}}}), the same inside {@code "dojo"}, or a bare
     * {@code {"url","apikey"}}; the pasted JSON wins when both are given.
     *
     * @throws IllegalArgumentException with a message for the user
     */
    public static Dojo parseDojo(String pastedJson, String url, String apiKey) {
        String useUrl = url;
        String useKey = apiKey;
        if (pastedJson != null && !pastedJson.isBlank()) {
            JSONObject root;
            try {
                root = new JSONObject(pastedJson.trim());
            } catch (Exception e) {
                throw new IllegalArgumentException("The Dojo pairing text is not valid JSON.");
            }
            JSONObject holder = root;
            if (root.optJSONObject("dojo") != null) {
                holder = root.getJSONObject("dojo");
            }
            if (holder.optJSONObject("pairing") != null) {
                holder = holder.getJSONObject("pairing");
            }
            useUrl = holder.optString("url", "");
            useKey = holder.optString("apikey", "");
        }
        useUrl = useUrl == null ? "" : useUrl.trim();
        useKey = useKey == null ? "" : useKey.trim();
        if (useUrl.isEmpty() || useKey.isEmpty()) {
            throw new IllegalArgumentException("Enter the Dojo's URL and API key, or paste its pairing JSON.");
        }
        String lower = useUrl.toLowerCase(Locale.ROOT);
        if (!lower.startsWith("http://") && !lower.startsWith("https://")) {
            throw new IllegalArgumentException("The Dojo URL must start with http:// or https://.");
        }
        return new Dojo(useUrl, useKey);
    }

    /** A pairing payload opened with its passphrase. {@code dojo} is null when the payload has none. */
    public record PayloadRead(String mnemonic, String bip39Passphrase, boolean hasPassphrase, boolean testnet, Dojo dojo) {}

    /**
     * Opens a pairing payload ({@code samourai.wallet.full}): decrypts the words with the passphrase
     * and reads the network and the Dojo. Ashigaru versions differ in how they write the Dojo: nested
     * ({@code dojo.pairing.{url,apikey}}) with a plain key, or flat ({@code dojo.{url,apikey}}) with the
     * key encrypted under the passphrase; both are read.
     *
     * @throws IllegalArgumentException with a message for the user
     */
    public static PayloadRead readPairingPayload(String payload, String passphrase) {
        JSONObject root;
        try {
            root = new JSONObject(payload == null ? "" : payload.trim());
        } catch (Exception e) {
            throw new IllegalArgumentException("This is not a pairing payload.");
        }
        JSONObject pairing = root.optJSONObject("pairing");
        if (pairing == null || !pairing.has("mnemonic") || !pairing.has("network")) {
            throw new IllegalArgumentException("This is not a pairing payload.");
        }
        if (passphrase == null || passphrase.isEmpty()) {
            throw new IllegalArgumentException("Enter the wallet's passphrase.");
        }
        String words;
        try {
            words = normalizeWords(AESUtil.decrypt(pairing.getString("mnemonic"), new CharSequenceX(passphrase)));
        } catch (Exception e) {
            throw new IllegalArgumentException("Wrong passphrase, or the payload is damaged.");
        }
        if (checkWords(words) != null) {
            throw new IllegalArgumentException("Wrong passphrase, or the payload is damaged.");
        }
        boolean hasPassphrase = pairing.optBoolean("passphrase", false);
        boolean testnet = "testnet".equalsIgnoreCase(pairing.getString("network"));

        Dojo dojo = null;
        JSONObject dojoBlock = root.optJSONObject("dojo");
        if (dojoBlock != null) {
            JSONObject holder = dojoBlock.optJSONObject("pairing") != null ? dojoBlock.getJSONObject("pairing") : dojoBlock;
            String url = holder.optString("url", "").trim();
            String key = holder.optString("apikey", "").trim();
            if (!url.isEmpty() && !key.isEmpty()) {
                try {
                    String decrypted = AESUtil.decrypt(key, new CharSequenceX(passphrase));
                    if (!decrypted.isBlank()) {
                        key = decrypted.trim(); // the flat form keeps the key encrypted
                    }
                } catch (Exception plainKey) {
                    // a plain key is not valid ciphertext: keep it as it is
                }
                try {
                    dojo = parseDojo("", url, key);
                } catch (IllegalArgumentException ignored) {
                    dojo = null;
                }
            }
        }
        return new PayloadRead(words, hasPassphrase ? passphrase : "", hasPassphrase, testnet, dojo);
    }

    /**
     * The pairing record in the shape {@code SwapsPairing} reads. {@code secret} encrypts the words
     * on this computer and is what the user types to unlock the app: the wallet's BIP39 passphrase
     * when it has one (as in an Ashigaru pairing payload), otherwise a password chosen here.
     */
    /** The app password is all that protects a seed without a BIP39 passphrase on this computer. */
    public static final int MIN_APP_PASSWORD_LENGTH = 8;

    /** What is wrong with the chosen app password, or {@code null} when it is fine. */
    public static String appPasswordProblem(String password, String repeat) {
        if (password == null || password.isEmpty()) return "Choose a password for this app.";
        if (password.length() < MIN_APP_PASSWORD_LENGTH) {
            return "Use at least " + MIN_APP_PASSWORD_LENGTH + " characters: this password is what protects the wallet on this computer.";
        }
        if (repeat == null || repeat.isEmpty()) return "Repeat the password.";
        if (!password.equals(repeat)) return "The two passwords differ.";
        return null;
    }

    // ---- a new wallet, made the way Ashigaru makes one: 12 words and a required BIP39 passphrase ----------

    /** How many words a new wallet has (128 bits of entropy), as in Ashigaru. */
    public static final int NEW_WALLET_WORD_COUNT = 12;

    /** How many of the words the user must type back to show they were written down. */
    public static final int CONFIRM_WORD_COUNT = 3;

    /**
     * A fresh 12-word BIP39 mnemonic (128 bits of entropy, as Ashigaru makes one).
     *
     * <p>The entropy comes from the operating system's cryptographic generator, read through two
     * independent Java generators: the platform's strongest ({@code getInstanceStrong}: /dev/random
     * on macOS and Linux, the Windows CSPRNG or DRBG on Windows) and a separately instantiated
     * SHA-512 DRBG with prediction resistance (reseeded from the OS on every request). Both outputs
     * are hashed together, so a flaw in either Java generator alone cannot weaken the seed. Never a
     * non-cryptographic or time-seeded generator (the "Milk Sad" failure).
     *
     * @throws IllegalStateException when a generator is plainly broken; no seed is made then
     */
    public static String generateWords() {
        return generateWords(strongRandom(), predictionResistantDrbg());
    }

    private static java.security.SecureRandom strongRandom() {
        try {
            return java.security.SecureRandom.getInstanceStrong();
        } catch (java.security.NoSuchAlgorithmException e) {
            return new java.security.SecureRandom(); // still the OS-seeded default CSPRNG
        }
    }

    private static java.security.SecureRandom predictionResistantDrbg() {
        try {
            return java.security.SecureRandom.getInstance("DRBG", java.security.DrbgParameters.instantiation(
                    256, java.security.DrbgParameters.Capability.PR_AND_RESEED,
                    "outpost-new-wallet".getBytes(java.nio.charset.StandardCharsets.UTF_8)));
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException("No DRBG available to make a seed", e);
        }
    }

    /** The mixing step, with the generators passed in so tests can check it and its guards. */
    static String generateWords(java.security.SecureRandom primary, java.security.SecureRandom secondary) {
        byte[] a = new byte[32];
        byte[] b = new byte[32];
        byte[] again = new byte[32];
        byte[] digest = null;
        byte[] entropy = null;
        try {
            primary.nextBytes(a);
            primary.nextBytes(again);
            secondary.nextBytes(b);
            // a working generator never repeats or returns one byte value 32 times (odds about 2^-248)
            if (isConstant(a) || isConstant(b) || java.util.Arrays.equals(a, again) || java.util.Arrays.equals(a, b)) {
                throw new IllegalStateException("The random number generator looks broken; no seed was made.");
            }
            java.security.MessageDigest sha = java.security.MessageDigest.getInstance("SHA-256");
            sha.update("outpost-bip39-entropy-v1".getBytes(java.nio.charset.StandardCharsets.UTF_8));
            sha.update(a);
            sha.update(b);
            digest = sha.digest();
            entropy = java.util.Arrays.copyOf(digest, 16);
            return generateWords(entropy);
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-256 is not available", e);
        } finally {
            java.util.Arrays.fill(a, (byte) 0);
            java.util.Arrays.fill(b, (byte) 0);
            java.util.Arrays.fill(again, (byte) 0);
            if (digest != null) java.util.Arrays.fill(digest, (byte) 0);
            if (entropy != null) java.util.Arrays.fill(entropy, (byte) 0);
        }
    }

    private static boolean isConstant(byte[] bytes) {
        for (byte value : bytes) {
            if (value != bytes[0]) return false;
        }
        return true;
    }

    /** The 12 words for 16 bytes of entropy (separate so tests can use known vectors). */
    public static String generateWords(byte[] entropy) {
        if (entropy == null || entropy.length != 16) {
            throw new IllegalArgumentException("A 12-word wallet needs exactly 16 bytes of entropy.");
        }
        try {
            return String.join(" ", MnemonicCode.INSTANCE.toMnemonic(entropy));
        } catch (MnemonicException e) {
            throw new IllegalStateException("Could not make the seed words", e);
        }
    }

    /** {@link #CONFIRM_WORD_COUNT} different word positions (0-based, ascending) to ask for. */
    public static int[] confirmationPositions(java.util.Random random) {
        java.util.TreeSet<Integer> chosen = new java.util.TreeSet<>();
        while (chosen.size() < CONFIRM_WORD_COUNT) {
            chosen.add(random.nextInt(NEW_WALLET_WORD_COUNT));
        }
        return chosen.stream().mapToInt(Integer::intValue).toArray();
    }

    /** True when {@code typed[i]} is the word of {@code mnemonic} at {@code positions[i]}. */
    public static boolean confirmationMatches(String mnemonic, int[] positions, String[] typed) {
        if (mnemonic == null || positions == null || typed == null || positions.length != typed.length) {
            return false;
        }
        String[] words = normalizeWords(mnemonic).split(" ");
        for (int i = 0; i < positions.length; i++) {
            if (positions[i] < 0 || positions[i] >= words.length) return false;
            if (typed[i] == null || !words[positions[i]].equals(typed[i].trim().toLowerCase(Locale.ROOT))) return false;
        }
        return true;
    }

    /** The passphrase of a new wallet is required, and typed twice (a typo would lock the funds away). */
    public static String newWalletPassphraseProblem(String passphrase, String repeat) {
        if (passphrase == null || passphrase.isEmpty()) return "Choose a passphrase: it is required, as in Ashigaru.";
        if (passphrase.length() < MIN_APP_PASSWORD_LENGTH) {
            return "Use a stronger passphrase: long, random, and not used anywhere else.";
        }
        if (repeat == null || repeat.isEmpty()) return "Repeat the passphrase.";
        if (!passphrase.equals(repeat)) return "The two passphrases differ.";
        return null;
    }

    public static String buildPairingPayload(String mnemonic, String secret, boolean secretIsBip39Passphrase, boolean testnet, Dojo dojo) {
        try {
            JSONObject pairing = new JSONObject();
            pairing.put("type", "samourai.wallet.full");
            pairing.put("version", "1.0.0");
            pairing.put("network", testnet ? "testnet" : "mainnet");
            pairing.put("passphrase", secretIsBip39Passphrase);
            pairing.put("mnemonic", AESUtil.encrypt(mnemonic, new CharSequenceX(secret), AESUtil.DefaultPBKDF2Iterations));

            JSONObject dojoPairing = new JSONObject();
            dojoPairing.put("type", "dojo.api");
            dojoPairing.put("version", "1.0.0");
            dojoPairing.put("url", dojo.url());
            dojoPairing.put("apikey", dojo.apiKey());

            return new JSONObject()
                    .put("pairing", pairing)
                    .put("dojo", new JSONObject().put("pairing", dojoPairing))
                    .toString();
        } catch (Exception e) {
            throw new IllegalStateException("Could not build the pairing record", e);
        }
    }
}
