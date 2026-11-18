package swap.gui.controller.pages;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import com.samourai.wallet.crypto.AESUtil;
import com.samourai.wallet.util.CharSequenceX;
import org.junit.Test;

public class SettingsSeedGuardTest {
    private static final String WORDS =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    @Test
    public void onlyThePassphraseTheAppUnlocksWithShowsTheSeed() throws Exception {
        String stored = AESUtil.encrypt(WORDS, new CharSequenceX("right passphrase"), AESUtil.DefaultPBKDF2Iterations);
        assertTrue(SettingsController.unlocksSeed(stored, "right passphrase"));
        assertFalse(SettingsController.unlocksSeed(stored, "wrong passphrase"));
        assertFalse(SettingsController.unlocksSeed(stored, ""));
        assertFalse(SettingsController.unlocksSeed(stored, null));
        assertFalse(SettingsController.unlocksSeed(null, "right passphrase"));
    }
}
