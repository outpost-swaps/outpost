package swap.gui.controller.pages;

import java.io.IOException;
import java.util.function.Consumer;
import javafx.scene.control.ComboBox;
import javafx.scene.control.ListCell;
import javafx.scene.control.RadioButton;
import swap.helper.HelperOwnNodes;
import swap.helper.HelperProperties;

/**
 * Ties a node dropdown to its "Public / My own" radio buttons. Whether a node is the user's own is
 * a property of the node (saved in a list), so the radios always describe the selected node, and
 * clicking one selects the node of that kind the user used last.
 */
final class NodeTypeSelector {
    private final ComboBox<String> combo;
    private final RadioButton own;
    private final RadioButton pub;
    private final String ownListKey;
    private final String lastOwnKey;
    private final String lastPublicKey;
    private final String flagKey;
    private final Consumer<Boolean> onTypeShown;
    private final Runnable addOwnNode;
    private final java.util.function.Function<String, String> sourceTag;
    private boolean syncing;

    NodeTypeSelector(ComboBox<String> combo, RadioButton own, RadioButton pub, String ownListKey,
                     String lastOwnKey, String lastPublicKey, String flagKey,
                     Consumer<Boolean> onTypeShown, Runnable addOwnNode,
                     java.util.function.Function<String, String> sourceTag) {
        this.combo = combo;
        this.own = own;
        this.pub = pub;
        this.ownListKey = ownListKey;
        this.lastOwnKey = lastOwnKey;
        this.lastPublicKey = lastPublicKey;
        this.flagKey = flagKey;
        this.onTypeShown = onTypeShown;
        this.addOwnNode = addOwnNode;
        this.sourceTag = sourceTag;
    }

    void install() {
        combo.setCellFactory(list -> new MarkedCell());
        combo.setButtonCell(new MarkedCell());
        sync(combo.getValue(), false);
        combo.valueProperty().addListener((obs, oldVal, newVal) -> sync(newVal, true));
        own.selectedProperty().addListener((obs, oldVal, newVal) -> {
            if (newVal && !syncing) {
                choose(true);
            }
        });
        pub.selectedProperty().addListener((obs, oldVal, newVal) -> {
            if (newVal && !syncing) {
                choose(false);
            }
        });
    }

    /** Marks a node as the user's own (or not) before it is selected. */
    void mark(String url, boolean isOwn) {
        save(ownListKey, HelperOwnNodes.with(HelperProperties.getProperty(ownListKey), url, isOwn));
    }

    private boolean isOwn(String url) {
        return HelperOwnNodes.isOwn(HelperProperties.getProperty(ownListKey), url);
    }

    private void choose(boolean wantOwn) {
        String target = HelperOwnNodes.pick(
                wantOwn,
                HelperProperties.getProperty(lastOwnKey),
                HelperProperties.getProperty(lastPublicKey),
                HelperProperties.getProperty(ownListKey),
                combo.getItems());
        if (target == null) {
            sync(combo.getValue(), false); // put the radios back on what is really selected
            if (wantOwn) {
                addOwnNode.run();
            }
        } else if (target.equals(combo.getValue())) {
            sync(target, false);
        } else {
            combo.setValue(target); // switches the node; the value listener syncs the radios
        }
    }

    private void sync(String url, boolean remember) {
        if (url == null) {
            return;
        }
        boolean ownNow = isOwn(url);
        syncing = true;
        try {
            own.setSelected(ownNow);
            pub.setSelected(!ownNow);
        } finally {
            syncing = false;
        }
        if (remember) {
            save(ownNow ? lastOwnKey : lastPublicKey, url);
        }
        if (!String.valueOf(ownNow).equals(HelperProperties.getProperty(flagKey))) {
            save(flagKey, String.valueOf(ownNow));
        }
        onTypeShown.accept(ownNow);
    }

    private static void save(String key, String value) {
        try {
            HelperProperties.setProperty(key, value);
        } catch (IOException e) {
            System.out.println("[SETTINGS] could not save " + key + ": " + e.getMessage());
        }
    }

    /** Shows a node's address, with "(personal)" after the user's own nodes. */
    private final class MarkedCell extends ListCell<String> {
        @Override
        protected void updateItem(String item, boolean empty) {
            super.updateItem(item, empty);
            setText(empty || item == null ? null : item + (isOwn(item) ? "   (personal)" : "") + sourceTag.apply(item));
        }
    }
}
