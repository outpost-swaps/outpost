package swap.gui;

import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.stream.Stream;
import javax.xml.parsers.DocumentBuilderFactory;
import org.junit.Test;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.Node;
import org.w3c.dom.NodeList;

/**
 * Only controls have a {@code tooltip} property. A tooltip on a layout pane (an HBox, 2026-11-12)
 * makes FXMLLoader fail and the whole page stays blank, and no other test loads the FXML.
 */
public class FxmlTooltipPlacementTest {
    private static final Set<String> CONTROLS = Set.of(
            "Label", "Button", "ToggleButton", "RadioButton", "CheckBox", "Hyperlink", "MenuButton", "SplitMenuButton",
            "TextField", "PasswordField", "TextArea", "ComboBox", "ChoiceBox", "Spinner", "Slider",
            "ListView", "TableView", "TreeView", "ProgressBar", "ProgressIndicator", "ScrollPane", "TitledPane");

    @Test
    public void everyTooltipIsOnAControl() throws Exception {
        List<String> wrong = new ArrayList<>();
        try (Stream<Path> files = Files.walk(Path.of("src/main/resources"))) {
            for (Path file : files.filter(p -> p.toString().endsWith(".fxml")).toList()) {
                DocumentBuilderFactory factory = DocumentBuilderFactory.newInstance();
                factory.setNamespaceAware(false);
                Document doc = factory.newDocumentBuilder().parse(file.toFile());
                NodeList tooltips = doc.getElementsByTagName("tooltip");
                for (int i = 0; i < tooltips.getLength(); i++) {
                    Node owner = tooltips.item(i).getParentNode();
                    String name = owner instanceof Element element ? element.getTagName() : "?";
                    if (!CONTROLS.contains(name)) {
                        wrong.add(file.getFileName() + ": <tooltip> inside <" + name + ">");
                    }
                }
            }
        }
        assertTrue("tooltips on non-controls: " + wrong, wrong.isEmpty());
    }
}
