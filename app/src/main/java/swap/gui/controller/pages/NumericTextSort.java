package swap.gui.controller.pages;

import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Orders table cells that hold text ("≈ 39%", "98.5%", "0.0004") by the number in them. Sorting the
 * text itself is alphabetical: "≈ 39%" came out between "≈ 5%" and "≈ 3%".
 */
final class NumericTextSort {
    private static final Pattern NUMBER = Pattern.compile("-?\\d+(?:\\.\\d+)?");

    private NumericTextSort() {}

    /** The first number in the text, or negative infinity when there is none (a "-" cell sorts lowest). */
    static double numberIn(String text) {
        if (text == null) {
            return Double.NEGATIVE_INFINITY;
        }
        Matcher matcher = NUMBER.matcher(text);
        return matcher.find() ? Double.parseDouble(matcher.group()) : Double.NEGATIVE_INFINITY;
    }

    static int compare(String left, String right) {
        return Double.compare(numberIn(left), numberIn(right));
    }
}
