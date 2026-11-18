package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import com.samourai.wallet.crypto.AESUtil;
import com.samourai.wallet.util.CharSequenceX;
import org.junit.Test;
import swap.model.SwapsPairing;

public class HelperWalletImportTest {
    // The BIP39 / BIP84 reference mnemonic. Published values: master fingerprint 73c5da0a, first
    // BIP84 receive address bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu (BIP-0084 specification).
    private static final String ABANDON =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    @Test
    public void theReferenceWalletHasThePublishedFingerprintAndFirstAddress() {
        HelperWalletImport.Verification v = HelperWalletImport.verify(ABANDON, "", false);

        assertEquals("73c5da0a", v.fingerprintHex());
        assertEquals("bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu", v.firstReceiveAddress());
    }

    @Test
    public void aDifferentPassphraseGivesADifferentWalletWithoutAnyError() {
        HelperWalletImport.Verification plain = HelperWalletImport.verify(ABANDON, "", false);
        HelperWalletImport.Verification typo = HelperWalletImport.verify(ABANDON, "testinG", false);

        assertFalse(plain.fingerprintHex().equals(typo.fingerprintHex()));
        assertFalse(plain.firstReceiveAddress().equals(typo.firstReceiveAddress()));
    }

    @Test
    public void testnetAddressesUseTheTestnetPrefix() {
        assertTrue(HelperWalletImport.firstReceiveAddress(ABANDON, "", true).startsWith("tb1q"));
        assertTrue(HelperWalletImport.firstReceiveAddress(ABANDON, "", false).startsWith("bc1q"));
    }

    @Test
    public void wordsAreCheckedForCountSpellingAndChecksum() {
        assertNull(HelperWalletImport.checkWords(ABANDON));
        assertTrue(HelperWalletImport.checkWords("").contains("Enter"));
        assertTrue(HelperWalletImport.checkWords("abandon abandon").contains("12 or 24"));
        assertTrue(HelperWalletImport.checkWords(ABANDON.replace("about", "abouts")).contains("abouts"));
        // valid words, wrong checksum
        assertTrue(HelperWalletImport.checkWords(ABANDON.replace("about", "abandon")).contains("checksum"));
    }

    @Test
    public void pastedWordsAreNormalised() {
        assertEquals(ABANDON, HelperWalletImport.normalizeWords("  Abandon abandon\nabandon  abandon abandon abandon\tabandon "
                + "abandon abandon abandon abandon About\n"));
    }

    @Test
    public void dojoIsReadFromItsOwnPairingJsonFromAWrappedOneOrFromFields() {
        String own = "{\"pairing\":{\"type\":\"dojo.api\",\"version\":\"1.4.5\",\"apikey\":\"key1\",\"url\":\"http://abc.onion/test/v2\"}}";
        String wrapped = "{\"dojo\":{\"pairing\":{\"apikey\":\"key1\",\"url\":\"http://abc.onion/test/v2\"}},\"explorer\":{}}";

        assertEquals(new HelperWalletImport.Dojo("http://abc.onion/test/v2", "key1"), HelperWalletImport.parseDojo(own, "", ""));
        assertEquals(new HelperWalletImport.Dojo("http://abc.onion/test/v2", "key1"), HelperWalletImport.parseDojo(wrapped, "", ""));
        assertEquals(new HelperWalletImport.Dojo("http://x.onion", "k"), HelperWalletImport.parseDojo("", " http://x.onion ", " k "));
    }

    @Test
    public void badDojoInputIsRejectedWithAMessage() {
        assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.parseDojo("not json", "", ""));
        assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.parseDojo("", "", ""));
        assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.parseDojo("", "http://x.onion", ""));
        assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.parseDojo("", "ftp://x.onion", "k"));
    }

    @Test
    public void theBuiltRecordIsReadBackByThePairingParserAndTheWordsDecryptWithTheSecret() throws Exception {
        String payload = HelperWalletImport.buildPairingPayload(
                ABANDON, "my-secret", true, true, new HelperWalletImport.Dojo("http://d.onion/test/v2", "dojo-key"));

        SwapsPairing pairing = new SwapsPairing(payload);

        assertEquals("TESTNET", pairing.getNetwork());
        assertTrue(pairing.hasPassphrase());
        assertEquals("http://d.onion/test/v2", pairing.getDojoUrl());
        assertEquals("dojo-key", pairing.getDojoApiKey());
        assertEquals(ABANDON, AESUtil.decrypt(pairing.getEncryptedSeed(), new CharSequenceX("my-secret")));
        assertTrue(HelperWallet.checkPassphrase(pairing.getEncryptedSeed(), "my-secret"));
        // a wrong passphrase must never be accepted, however the garbage happens to pad (the old check let ~6% through)
        for (int i = 0; i < 40; i++) {
            assertFalse(HelperWallet.checkPassphrase(pairing.getEncryptedSeed(), "wrong-" + i));
        }
    }

    @Test
    public void aWalletWithoutABip39PassphraseIsMarkedSoAndNetworkIsMainnet() throws Exception {
        String payload = HelperWalletImport.buildPairingPayload(
                ABANDON, "app-password", false, false, new HelperWalletImport.Dojo("https://d.example.org", "k"));

        SwapsPairing pairing = new SwapsPairing(payload);

        assertFalse(pairing.hasPassphrase());
        assertEquals("MAINNET", pairing.getNetwork());
        assertNotNull(pairing.getEncryptedSeed());
    }

    @Test
    public void aNestedPayloadWithAPlainDojoKeyIsOpened() throws Exception {
        String payload = HelperWalletImport.buildPairingPayload(
                ABANDON, "secret", true, true, new HelperWalletImport.Dojo("http://d.onion/test/v2", "plainkey"));

        HelperWalletImport.PayloadRead read = HelperWalletImport.readPairingPayload(payload, "secret");

        assertEquals(ABANDON, read.mnemonic());
        assertEquals("secret", read.bip39Passphrase());
        assertTrue(read.hasPassphrase());
        assertTrue(read.testnet());
        assertEquals(new HelperWalletImport.Dojo("http://d.onion/test/v2", "plainkey"), read.dojo());
    }

    @Test
    public void aFlatPayloadWithAnEncryptedDojoKeyIsOpenedToo() throws Exception {
        // the shape the mobile source writes: dojo.{url, apikey} with the key encrypted under the passphrase
        String encryptedWords = AESUtil.encrypt(ABANDON, new CharSequenceX("secret"), AESUtil.DefaultPBKDF2Iterations);
        String encryptedKey = AESUtil.encrypt("dojokey123", new CharSequenceX("secret"), AESUtil.DefaultPBKDF2Iterations);
        String payload = "{\"pairing\":{\"type\":\"samourai.wallet.full\",\"version\":\"1.0.0\",\"network\":\"mainnet\","
                + "\"passphrase\":true,\"mnemonic\":\"" + encryptedWords + "\"},"
                + "\"dojo\":{\"apikey\":\"" + encryptedKey + "\",\"url\":\"http://flat.onion/v2\"}}";

        HelperWalletImport.PayloadRead read = HelperWalletImport.readPairingPayload(payload, "secret");

        assertEquals(new HelperWalletImport.Dojo("http://flat.onion/v2", "dojokey123"), read.dojo());
        assertFalse(read.testnet());
    }

    @Test
    public void aPayloadWithoutBip39PassphraseUsesAnEmptyOneForTheWallet() throws Exception {
        String payload = HelperWalletImport.buildPairingPayload(
                ABANDON, "one-time-password", false, false, new HelperWalletImport.Dojo("http://d.onion/v2", "k"));

        HelperWalletImport.PayloadRead read = HelperWalletImport.readPairingPayload(payload, "one-time-password");

        assertFalse(read.hasPassphrase());
        assertEquals("", read.bip39Passphrase());
        // so the wallet it opens is the plain one: the reference fingerprint
        assertEquals("73c5da0a", HelperWalletImport.verify(read.mnemonic(), read.bip39Passphrase(), false).fingerprintHex());
    }

    @Test
    public void aPayloadWithNoDojoGivesNone() throws Exception {
        String encryptedWords = AESUtil.encrypt(ABANDON, new CharSequenceX("secret"), AESUtil.DefaultPBKDF2Iterations);
        String payload = "{\"pairing\":{\"network\":\"mainnet\",\"passphrase\":true,\"mnemonic\":\"" + encryptedWords + "\"}}";

        assertNull(HelperWalletImport.readPairingPayload(payload, "secret").dojo());
    }

    @Test
    public void aWrongPassphraseOrABrokenPayloadIsRejectedWithAMessage() throws Exception {
        String payload = HelperWalletImport.buildPairingPayload(ABANDON, "secret", true, false, new HelperWalletImport.Dojo("http://d.onion/v2", "k"));

        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.readPairingPayload(payload, "wrong")).getMessage().contains("Wrong passphrase"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.readPairingPayload("nope", "secret")).getMessage().contains("not a pairing payload"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.readPairingPayload("{}", "secret")).getMessage().contains("not a pairing payload"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.readPairingPayload(payload, "")).getMessage().contains("passphrase"));
    }

    /**
     * A wallet with no BIP39 passphrase, end to end: stored with "passphrase": false, encrypted with
     * the app password, unlocked with the app password, and derived with an EMPTY BIP39 passphrase,
     * so it is the same wallet the words give in Sparrow or any BIP39 wallet. Using the app password
     * as the BIP39 passphrase would silently derive a different wallet.
     */
    @Test
    public void aWalletWithoutAPassphraseUnlocksWithTheAppPasswordAndStaysTheSameWallet() throws Exception {
        String appPassword = "correct horse battery";
        String payload = HelperWalletImport.buildPairingPayload(
                ABANDON, appPassword, false, false, new HelperWalletImport.Dojo("https://d.example.org", "k"));
        SwapsPairing pairing = new SwapsPairing(payload);

        assertTrue(swap.helper.HelperWallet.checkPassphrase(pairing.getEncryptedSeed(), appPassword));
        assertFalse(swap.helper.HelperWallet.checkPassphrase(pairing.getEncryptedSeed(), "wrong password"));

        byte[] unlocked = swap.helper.HelperWallet
                .createKeyChain(pairing.getEncryptedSeed(), appPassword, pairing.hasPassphrase())
                .getSeed().getSeedBytes();
        java.util.List<String> words = java.util.Arrays.asList(ABANDON.split(" "));
        org.junit.Assert.assertArrayEquals(org.bitcoinj.crypto.MnemonicCode.toSeed(words, ""), unlocked);
        assertFalse(java.util.Arrays.equals(org.bitcoinj.crypto.MnemonicCode.toSeed(words, appPassword), unlocked));
    }

    /** With a BIP39 passphrase, the unlock secret is that passphrase and it is part of the wallet. */
    @Test
    public void aWalletWithAPassphraseUnlocksWithItAndUsesIt() throws Exception {
        String payload = HelperWalletImport.buildPairingPayload(
                ABANDON, "testing", true, false, new HelperWalletImport.Dojo("https://d.example.org", "k"));
        SwapsPairing pairing = new SwapsPairing(payload);
        byte[] unlocked = swap.helper.HelperWallet
                .createKeyChain(pairing.getEncryptedSeed(), "testing", pairing.hasPassphrase())
                .getSeed().getSeedBytes();
        org.junit.Assert.assertArrayEquals(
                org.bitcoinj.crypto.MnemonicCode.toSeed(java.util.Arrays.asList(ABANDON.split(" ")), "testing"), unlocked);
    }

    @Test
    public void theAppPasswordNeedsEightCharactersAndTheSameRepeat() {
        assertEquals("Choose a password for this app.", HelperWalletImport.appPasswordProblem("", ""));
        assertTrue(HelperWalletImport.appPasswordProblem("short12", "short12").startsWith("Use at least 8 characters"));
        assertEquals("Repeat the password.", HelperWalletImport.appPasswordProblem("long enough", ""));
        assertEquals("The two passwords differ.", HelperWalletImport.appPasswordProblem("long enough", "long enougH"));
        assertNull(HelperWalletImport.appPasswordProblem("long enough", "long enough"));
    }

    // ---- a new wallet: 12 words and a required passphrase, as Ashigaru makes one ----

    @Test
    public void newWalletWordsAreTheBip39WordsOfSixteenBytesOfEntropy() {
        // BIP39 reference vectors (Trezor): all-zero and all-0x7f entropy.
        assertEquals(ABANDON, HelperWalletImport.generateWords(new byte[16]));
        byte[] sevenF = new byte[16];
        java.util.Arrays.fill(sevenF, (byte) 0x7f);
        assertEquals("legal winner thank year wave sausage worth useful legal winner thank yellow",
                HelperWalletImport.generateWords(sevenF));
        assertThrows(IllegalArgumentException.class, () -> HelperWalletImport.generateWords(new byte[32]));
    }

    @Test
    public void randomNewWalletsHaveTwelveValidWordsAndDiffer() {
        String a = HelperWalletImport.generateWords();
        String b = HelperWalletImport.generateWords();
        assertEquals(12, a.split(" ").length);
        assertNull(HelperWalletImport.checkWords(a));
        assertNull(HelperWalletImport.checkWords(b));
        assertFalse(a.equals(b));
    }

    @Test
    public void aNewWalletOpensLikeAnyOtherAndKeepsItsPassphrase() {
        // Same words, different passphrase: a different wallet, so the passphrase is part of the wallet.
        String words = HelperWalletImport.generateWords();
        assertFalse(HelperWalletImport.verify(words, "first passphrase", false).fingerprintHex()
                .equals(HelperWalletImport.verify(words, "other passphrase", false).fingerprintHex()));
    }

    @Test
    public void confirmationAsksForThreeDifferentWordsInOrderAndChecksThem() {
        int[] positions = HelperWalletImport.confirmationPositions(new java.util.Random(7));
        assertEquals(3, positions.length);
        assertTrue(positions[0] < positions[1] && positions[1] < positions[2]);
        assertTrue(positions[0] >= 0 && positions[2] < 12);

        String[] words = ABANDON.split(" ");
        String[] right = {words[positions[0]], " " + words[positions[1]].toUpperCase() + " ", words[positions[2]]};
        assertTrue(HelperWalletImport.confirmationMatches(ABANDON, positions, right));
        assertFalse(HelperWalletImport.confirmationMatches(ABANDON, new int[] {0, 1, 11}, new String[] {"abandon", "abandon", "abandon"}));
        assertFalse(HelperWalletImport.confirmationMatches(ABANDON, new int[] {0, 1, 2}, new String[] {"abandon", "abandon", ""}));
        assertFalse(HelperWalletImport.confirmationMatches(ABANDON, new int[] {0, 1, 2}, new String[] {"abandon", "abandon"}));
    }

    @Test
    public void theNewWalletPassphraseIsRequiredLongEnoughAndRepeated() {
        assertTrue(HelperWalletImport.newWalletPassphraseProblem("", "").startsWith("Choose a passphrase"));
        assertTrue(HelperWalletImport.newWalletPassphraseProblem("short12", "short12").startsWith("Use a stronger passphrase"));
        assertEquals("Repeat the passphrase.", HelperWalletImport.newWalletPassphraseProblem("long enough", ""));
        assertEquals("The two passphrases differ.", HelperWalletImport.newWalletPassphraseProblem("long enough", "long enougH"));
        assertNull(HelperWalletImport.newWalletPassphraseProblem("long enough", "long enough"));
    }

    // ---- the entropy for a new wallet ----

    /** Returns the same byte forever: a stubbed or failed generator. */
    private static java.security.SecureRandom constant(byte value) {
        return new java.security.SecureRandom() {
            @Override
            public void nextBytes(byte[] bytes) {
                java.util.Arrays.fill(bytes, value);
            }
        };
    }

    /** Returns the same 32 "random" bytes on every call: a generator stuck on one output. */
    private static java.security.SecureRandom repeating() {
        return new java.security.SecureRandom() {
            @Override
            public void nextBytes(byte[] bytes) {
                for (int i = 0; i < bytes.length; i++) bytes[i] = (byte) (i * 7 + 1);
            }
        };
    }

    @Test
    public void aBrokenGeneratorMakesNoSeed() {
        java.security.SecureRandom good = new java.security.SecureRandom();
        assertThrows(IllegalStateException.class, () -> HelperWalletImport.generateWords(constant((byte) 0), good));
        assertThrows(IllegalStateException.class, () -> HelperWalletImport.generateWords(good, constant((byte) 0x41)));
        assertThrows(IllegalStateException.class, () -> HelperWalletImport.generateWords(repeating(), good));
    }

    /** A deterministic generator: two of these give the same bytes. */
    private static java.security.SecureRandom seeded() {
        try {
            java.security.SecureRandom random = java.security.SecureRandom.getInstance("SHA1PRNG");
            random.setSeed(new byte[] {1, 2, 3});
            return random;
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new AssertionError(e);
        }
    }

    @Test
    public void bothGeneratorsCountSoOneAloneDoesNotDecideTheSeed() {
        // the same primary output with two different secondary outputs gives two different seeds
        java.security.SecureRandom firstPrimary = seeded();
        java.security.SecureRandom secondPrimary = seeded();
        String one = HelperWalletImport.generateWords(firstPrimary, new java.security.SecureRandom());
        String two = HelperWalletImport.generateWords(secondPrimary, new java.security.SecureRandom());
        assertFalse(one.equals(two));
        assertNull(HelperWalletImport.checkWords(one));
    }

    @Test
    public void manyNewWalletsAreAllDifferentAndUseTheWholeWordList() {
        java.util.Set<String> seeds = new java.util.HashSet<>();
        java.util.Set<String> firstWords = new java.util.HashSet<>();
        for (int i = 0; i < 300; i++) {
            String words = HelperWalletImport.generateWords();
            assertTrue(seeds.add(words));
            firstWords.add(words.split(" ")[0]);
        }
        // 300 draws from 2048 first words: a working generator gives ~280 different ones, a weak one far fewer
        assertTrue("only " + firstWords.size() + " different first words", firstWords.size() > 230);
    }
}
