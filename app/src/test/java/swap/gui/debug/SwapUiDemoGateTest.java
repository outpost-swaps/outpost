package swap.gui.debug;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** The UI demo is developer-only: the environment variable alone turns it on (no data-folder file). */
public class SwapUiDemoGateTest {
    @Test
    public void onlyTheEnvironmentVariableEnablesIt() {
        assertTrue(SwapUiDemo.isEnabledBy("1"));
        assertFalse(SwapUiDemo.isEnabledBy(null));
        assertFalse(SwapUiDemo.isEnabledBy("0"));
    }

    @Test
    public void theDataFolderFileTriggerIsGone() throws Exception {
        String source = new String(java.nio.file.Files.readAllBytes(java.nio.file.Paths.get(
                "src/main/java/swap/gui/debug/SwapUiDemo.java")));
        assertFalse(source.contains("\"gui-ui-demo\""));
    }
}
