package swap.gui.controller.popups;

import java.util.List;
import java.util.Map;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.geometry.Pos;
import javafx.scene.Parent;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.Label;
import javafx.scene.control.Tooltip;
import javafx.scene.layout.HBox;
import javafx.scene.layout.Priority;
import javafx.scene.layout.Region;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.scene.text.Font;
import javafx.stage.Stage;
import swap.gui.GUISwap;

/** Every swap the ASB is running, newest first, opened from "+N more" on the Liquidity card. */
public class PopupSwapsInProgress {

    @FXML
    private Label subtitleText;
    @FXML
    private VBox swapsBox;
    @FXML
    private Button closeButton;

    /** The subtitle: how many swaps, in words. */
    static String subtitle(int count) {
        return count == 1 ? "1 swap is running." : count + " swaps are running.";
    }

    /** @param swaps swap id to step label, oldest first as the ASB reported them */
    public static void show(Stage owner, List<Map.Entry<String, String>> swaps) {
        try {
            FXMLLoader loader = new FXMLLoader(GUISwap.class.getResource("/popups/popup-swaps-in-progress.fxml"));
            Parent root = loader.load();
            Stage stage = new Stage();
            if (owner != null) {
                stage.initOwner(owner);
            }
            stage.setTitle("Swaps in progress");
            Scene scene = new Scene(root, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);
            stage.setResizable(false);
            PopupSwapsInProgress popup = loader.getController();
            popup.fill(swaps);
            stage.show();
            stage.requestFocus();
            stage.toFront();
        } catch (Exception e) {
            e.printStackTrace();
        }
    }

    private void fill(List<Map.Entry<String, String>> swaps) {
        subtitleText.setText(subtitle(swaps.size()));
        for (int i = swaps.size() - 1; i >= 0; i--) {
            Map.Entry<String, String> swap = swaps.get(i);
            String id = swap.getKey();
            Label idLabel = new Label(id.length() > 8 ? id.substring(0, 8) + "…" : id);
            idLabel.setFont(Font.font("Menlo", 13));
            idLabel.setTextFill(Color.web("#e7eaf0"));
            idLabel.setTooltip(new Tooltip(id));
            Label step = new Label(swap.getValue());
            step.setFont(Font.font("Arial", 13));
            step.setTextFill(Color.web("#be7327"));
            Region spacer = new Region();
            HBox.setHgrow(spacer, Priority.ALWAYS);
            HBox row = new HBox(10, idLabel, spacer, step);
            row.setAlignment(Pos.CENTER_LEFT);
            swapsBox.getChildren().add(row);
        }
    }

    @FXML
    private void closeWindow() {
        ((Stage) closeButton.getScene().getWindow()).close();
    }
}
