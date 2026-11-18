package swap.process;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.io.File;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.stream.Stream;
import org.junit.Test;

public class ProcessAsbLogPathTest {
    @Test
    public void theAsbLogIsInTheAppDataFolder() {
        assertEquals(new File("/data/outpost/logs/asb.log").toPath(), ProcessAsb.asbLogPath(new File("/data/outpost")));
    }

    /**
     * A path like ~/Library/Application Support is macOS-only and ignores the data folder the app is
     * really using; everything must go through AppSwap.getSwapRootDir().
     */
    @Test
    public void noSourceFileHardcodesAMacOsDataPath() throws Exception {
        Path root = Path.of("src/main/java");
        List<String> offenders = new ArrayList<>();
        try (Stream<Path> files = Files.walk(root)) {
            for (Path file : (Iterable<Path>) files.filter(p -> p.toString().endsWith(".java"))::iterator) {
                String text = Files.readString(file);
                if (text.contains("\"Library\"") && text.contains("\"Application Support\"")) {
                    offenders.add(file.toString());
                }
            }
        }
        assertTrue("hardcoded macOS data path in " + offenders, offenders.isEmpty());
    }
}
