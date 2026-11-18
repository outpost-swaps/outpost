package swap.gui.controller.pages;

import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class NumericTextSortTest {

    @Test
    public void spreadSortsByNumberNotAlphabetically() {
        // alphabetically "≈ 39%" came out between "≈ 5%" and "≈ 3%"
        assertTrue(NumericTextSort.compare("≈ 39%", "≈ 5%") > 0);
        assertTrue(NumericTextSort.compare("≈ 5%", "≈ 3%") > 0);
        assertTrue(NumericTextSort.compare("≈ 3%", "≈ 2%") > 0);
        assertTrue(NumericTextSort.compare("≈ -3%", "≈ 2%") < 0);
    }

    @Test
    public void refundAndAmountsSortByNumber() {
        assertTrue(NumericTextSort.compare("100%", "98.5%") > 0);
        assertTrue(NumericTextSort.compare("10", "5") > 0);
        assertTrue(NumericTextSort.compare("0.0004", "0.005") < 0);
    }

    @Test
    public void aDashSortsLowest() {
        assertTrue(NumericTextSort.compare("-", "≈ -3%") < 0);
        assertTrue(NumericTextSort.compare("-", "-") == 0);
        assertTrue(NumericTextSort.compare(null, "0") < 0);
    }
}
