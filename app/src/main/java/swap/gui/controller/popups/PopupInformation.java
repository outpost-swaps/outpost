package swap.gui.controller.popups;

import java.util.ArrayList;
import java.util.List;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.geometry.Pos;
import javafx.scene.Parent;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.Label;
import javafx.scene.layout.HBox;
import javafx.scene.layout.Priority;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.stage.Stage;
import swap.gui.GUISwap;

/** The "What's New" window: a themed list of changes with a footer note. */
public class PopupInformation {

    @FXML
    private Label titleText;
    @FXML
    private Label subtitleText;
    @FXML
    private VBox itemsCard;
    @FXML
    private VBox itemsBox;
    @FXML
    private VBox noteCard;
    @FXML
    private Label noteText;
    @FXML
    private Button closeButton;

    /** The text split into its bullet lines and the paragraph(s) that are not bullets. */
    record Parsed(List<String> items, String note) {}

    /**
     * Lines starting with "- " are bullets (the dash is dropped); the other non-empty lines, in
     * order, form the note.
     */
    static Parsed parse(String text) {
        List<String> items = new ArrayList<>();
        StringBuilder note = new StringBuilder();
        if (text != null) {
            for (String raw : text.split("\\R")) {
                String line = raw.strip();
                if (line.isEmpty()) {
                    continue;
                }
                if (line.startsWith("- ")) {
                    items.add(line.substring(2).strip());
                } else {
                    if (note.length() > 0) {
                        note.append(' ');
                    }
                    note.append(line);
                }
            }
        }
        return new Parsed(items, note.toString());
    }

    public static PopupInformation create(String title, String innerTitleText, String bodyText) {
        try {
            FXMLLoader fxmlLoader = new FXMLLoader(GUISwap.class.getResource("/popups/popup-information.fxml"));
            Parent root = fxmlLoader.load();
            Stage stage = new Stage();
            stage.setTitle(title);
            Scene scene = new Scene(root, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);
            stage.setResizable(false);
            PopupInformation popup = fxmlLoader.getController();
            popup.fill(innerTitleText, bodyText);
            stage.show();
            stage.requestFocus();
            stage.toFront();
            stage.setAlwaysOnTop(true);
            return popup;
        } catch (Exception e) {
            e.printStackTrace();
            return null;
        }
    }

    @FXML
    private void closeWindow() {
        Stage stage = (Stage) closeButton.getScene().getWindow();
        stage.close();
    }

    private void fill(String heading, String body) {
        titleText.setText(heading);
        subtitleText.setText("Version " + GUISwap.VERSION);
        Parsed parsed = parse(body);
        for (String item : parsed.items()) {
            Label dot = new Label("•");
            dot.setTextFill(Color.web("#8f1a14"));
            Label text = new Label(item);
            text.setWrapText(true);
            text.setMaxWidth(Double.MAX_VALUE);
            HBox row = new HBox(10, dot, text);
            row.setAlignment(Pos.TOP_LEFT);
            HBox.setHgrow(text, Priority.ALWAYS);
            itemsBox.getChildren().add(row);
        }
        itemsCard.setVisible(!parsed.items().isEmpty());
        itemsCard.setManaged(!parsed.items().isEmpty());
        noteText.setText(parsed.note());
        noteCard.setVisible(!parsed.note().isEmpty());
        noteCard.setManaged(!parsed.note().isEmpty());
    }
}
