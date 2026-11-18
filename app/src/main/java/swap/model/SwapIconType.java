package swap.model;

import javafx.scene.paint.Color;

public enum SwapIconType {
    INITIATED("EXCHANGE", Color.WHITE, 80),
    BTC_LOCK("LOCK", Color.WHITE, 80),
    REFUND_PENDING("HOURGLASS", Color.web("#be7327"), 60),
    CANCEL_PENDING("CLOSE", Color.web("#be7327"), 80),
    XMR_LOCK("LOCK", Color.web("#8f1a14"), 80),
    BTC_REDEEM("UNLOCK", Color.web("#8f1a14"), 80),
    SYNCING("REFRESH", Color.WHITE, 70),
    SWEEPING("PAPER_PLANE", Color.WHITE, 64),
    CHECK("CHECK", Color.web("#27be4d"), 80),
    CLOSE("CLOSE", Color.web("#c0201a"), 80);

    private final Color color;
    private final String glyphName;
    private final int size;

    SwapIconType(String glyphName, Color color, int size) {
        this.glyphName = glyphName;
        this.color = color;
        this.size = size;
    }

    public Color getColor() {
        return color;
    }

    public String getGlyphName() {
        return glyphName;
    }

    public int getSize() {
        return size;
    }
}
