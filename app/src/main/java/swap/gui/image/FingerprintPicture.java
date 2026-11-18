package swap.gui.image;

import com.sparrowwallet.toucan.LifeHash;
import com.sparrowwallet.toucan.LifeHashVersion;
import java.awt.RenderingHints;
import java.awt.image.BufferedImage;
import javafx.embed.swing.SwingFXUtils;
import javafx.scene.image.Image;

/**
 * The LifeHash picture Ashigaru Desktop and Sparrow show for a wallet's master fingerprint, so a
 * wallet can be recognised at a glance (and in the same way) across those apps.
 */
public final class FingerprintPicture {
    private FingerprintPicture() {}

    /** A square image of {@code pixels} for the 4-byte fingerprint; same bytes, same picture. */
    public static Image of(byte[] fingerprint, int pixels) {
        LifeHash.Image lifeHash = LifeHash.makeFromData(fingerprint, LifeHashVersion.VERSION2, 1, false);
        BufferedImage source = LifeHash.getBufferedImage(lifeHash);
        BufferedImage scaled = new BufferedImage(pixels, pixels, BufferedImage.TYPE_INT_ARGB);
        java.awt.Graphics2D graphics = scaled.createGraphics();
        try {
            // nearest neighbour keeps the cells crisp, as the desktop wallets draw them
            graphics.setRenderingHint(RenderingHints.KEY_INTERPOLATION, RenderingHints.VALUE_INTERPOLATION_NEAREST_NEIGHBOR);
            graphics.drawImage(source, 0, 0, pixels, pixels, null);
        } finally {
            graphics.dispose();
        }
        return SwingFXUtils.toFXImage(scaled, null);
    }
}
