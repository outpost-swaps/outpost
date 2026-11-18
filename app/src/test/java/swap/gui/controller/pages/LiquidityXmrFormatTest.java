package swap.gui.controller.pages;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class LiquidityXmrFormatTest {
    @Test
    public void everyXmrFigureHasSixDecimalsRoundedDown() {
        assertEquals("0.565945", LiquidityController.fmtXmr(0.5659451));
        assertEquals("0.565945", LiquidityController.fmtXmr(0.5659459)); // never rounds up past what is there
        assertEquals("1.000000", LiquidityController.fmtXmr(1));
        assertEquals("0.000000", LiquidityController.fmtXmr(0));
    }
}
