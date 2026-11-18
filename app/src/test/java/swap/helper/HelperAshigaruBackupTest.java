package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import com.samourai.wallet.crypto.AESUtil;
import com.samourai.wallet.util.CharSequenceX;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import org.json.JSONObject;
import org.junit.Test;
import swap.helper.HelperAshigaruBackup.BackupException;
import swap.helper.HelperAshigaruBackup.Imported;

/**
 * The fixture is a made-up wallet (the published BIP39 test-vector entropy, a fake Dojo key) written
 * by an independent implementation (Python plus OpenSSL), so it checks the file format and not
 * just that this code agrees with itself. It ends in stray bytes, like a real Ashigaru backup does.
 */
public class HelperAshigaruBackupTest {
    private static final String PASS = "fixture-pass";
    private static final String WORDS = "army van defense carry jealous true garbage claim echo media make crunch";
    private static final String FINGERPRINT = "43f9382e"; // standard BIP32 form; the backup stores it reversed (2e38f943)

    private static String fixture() throws Exception {
        try (InputStream in = HelperAshigaruBackupTest.class.getResourceAsStream("/ashigaru/backup-v2-fixture.txt")) {
            return new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }
    }

    @Test
    public void readsTheWalletAndDojoFromAVersion2BackupWithStrayBytesAtTheEnd() throws Exception {
        Imported imported = HelperAshigaruBackup.read(fixture(), PASS);

        assertEquals(WORDS, imported.mnemonic());
        assertEquals(PASS, imported.passphrase());
        assertTrue(imported.testnet());
        assertEquals(FINGERPRINT, imported.fingerprint());
        assertTrue(imported.dojoUrl().endsWith(".onion/test/v2"));
        assertEquals("0123456789abcdef0123456789abcdef01234567", imported.dojoApiKey());
    }

    @Test
    public void aWrongPassphraseIsRejectedWithAClearMessage() throws Exception {
        BackupException e = assertThrows(BackupException.class, () -> HelperAshigaruBackup.read(fixture(), "not-the-passphrase"));
        assertTrue(e.getMessage().contains("Wrong passphrase"));
    }

    @Test
    public void aFileThatIsNotABackupIsRejected() {
        assertThrows(BackupException.class, () -> HelperAshigaruBackup.read("hello", PASS));
        assertThrows(BackupException.class, () -> HelperAshigaruBackup.read("{\"version\":2}", PASS));
        assertThrows(BackupException.class, () -> HelperAshigaruBackup.read("", PASS));
    }

    @Test
    public void anUnknownVersionSaysSo() {
        BackupException e = assertThrows(BackupException.class,
                () -> HelperAshigaruBackup.read("{\"version\":9,\"payload\":\"x\",\"external\":false}", PASS));
        assertTrue(e.getMessage().contains("version 9"));
    }

    @Test
    public void aMissingPassphraseAsksForIt() throws Exception {
        assertThrows(BackupException.class, () -> HelperAshigaruBackup.read(fixture(), ""));
    }

    @Test
    public void aBackupWhoseFingerprintDoesNotMatchTheSeedIsRejected() throws Exception {
        // Same words and passphrase, but the fingerprint field says another wallet: damaged or swapped.
        String plain = "{\"wallet\":{\"testnet\":true,\"seed\":\"0c1e24e5917779d297e14d45f14e1a1a\","
                + "\"passphrase\":\"" + PASS + "\",\"fingerprint\":\"deadbeef\"}}";
        String wrapper = "{\"version\":1,\"payload\":\"" + AESUtil.encrypt(plain, new CharSequenceX(PASS), AESUtil.DefaultPBKDF2Iterations)
                + "\",\"external\":false}";
        assertThrows(BackupException.class, () -> HelperAshigaruBackup.read(wrapper, PASS));
    }

    @Test
    public void aVersion1BackupWithoutDojoAndWithoutAFingerprintStillImports() throws Exception {
        String plain = "{\"wallet\":{\"testnet\":false,\"seed\":\"0c1e24e5917779d297e14d45f14e1a1a\",\"passphrase\":\"" + PASS + "\"}}";
        String wrapper = "{\"version\":1,\"payload\":\"" + AESUtil.encrypt(plain, new CharSequenceX(PASS), AESUtil.DefaultPBKDF2Iterations)
                + "\",\"external\":false}";

        Imported imported = HelperAshigaruBackup.read(wrapper, PASS);

        assertEquals(WORDS, imported.mnemonic());
        assertFalse(imported.testnet());
        assertNull(imported.dojoUrl());
        assertNull(imported.dojoApiKey());
        assertEquals(FINGERPRINT, imported.fingerprint());
    }

    @Test
    public void theSecretsNeverAppearInTheTextForm() throws Exception {
        String text = HelperAshigaruBackup.read(fixture(), PASS).toString();
        assertFalse(text.contains("army"));
        assertFalse(text.contains(PASS));
        assertFalse(text.contains("0123456789abcdef"));
        assertTrue(new JSONObject("{\"a\":1}").has("a"));
    }

    // ---- writing a backup for a wallet made in Outpost ----

    @Test
    public void aWrittenBackupReadsBackToTheSameWalletAndDojo() throws Exception {
        String file = HelperAshigaruBackup.write(WORDS, PASS, true, new HelperWalletImport.Dojo("http://dojo.onion/v2", "key-123"));
        Imported imported = HelperAshigaruBackup.read(file, PASS);

        assertEquals(WORDS, imported.mnemonic());
        assertEquals(PASS, imported.passphrase());
        assertTrue(imported.testnet());
        assertEquals(FINGERPRINT, imported.fingerprint());
        assertEquals("http://dojo.onion/v2", imported.dojoUrl());
        assertEquals("key-123", imported.dojoApiKey());
    }

    @Test
    public void aWrittenBackupHasAshigarusLayout() throws Exception {
        JSONObject wrapper = new JSONObject(HelperAshigaruBackup.write(WORDS, PASS, false, null));
        assertEquals(2, wrapper.getInt("version"));
        assertTrue(wrapper.getBoolean("external"));
        String payload = wrapper.getString("payload");
        assertTrue(payload.startsWith("U2FsdGVkX1")); // base64 of "Salted__"
        for (String line : payload.split("\n")) {
            assertTrue(line.length() <= 64);
        }
        // the inner JSON: entropy hex as the seed and the byte-reversed fingerprint, as Ashigaru writes them
        JSONObject wallet = new JSONObject(HelperAshigaruBackup.decryptOpenSslSha256(payload, PASS)).getJSONObject("wallet");
        assertEquals(32, wallet.getString("seed").length());
        assertEquals("2e38f943", wallet.getString("fingerprint"));
        assertFalse(wallet.getBoolean("testnet"));
        assertFalse(HelperAshigaruBackup.read(HelperAshigaruBackup.write(WORDS, PASS, false, null), PASS).dojoUrl() != null);
    }

    @Test
    public void aWrittenBackupNeedsTheRightPassphraseAndUsesAFreshSalt() throws Exception {
        String a = HelperAshigaruBackup.write(WORDS, PASS, true, null);
        String b = HelperAshigaruBackup.write(WORDS, PASS, true, null);
        assertFalse(a.equals(b));
        assertThrows(BackupException.class, () -> HelperAshigaruBackup.read(a, "wrong-pass"));
        assertThrows(IllegalArgumentException.class, () -> HelperAshigaruBackup.write(WORDS, "", true, null));
    }
}
