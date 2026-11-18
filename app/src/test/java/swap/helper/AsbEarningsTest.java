package swap.helper;

import org.json.JSONObject;
import org.junit.Test;

import java.util.List;
import java.util.Map;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public class AsbEarningsTest {

    @Test
    public void aSwapIsCountedOnlyOnce() {
        AsbEarnings earnings = new AsbEarnings(new JSONObject());
        assertTrue(earnings.record("swap-a", 99_457));
        assertFalse(earnings.record("swap-a", 99_457));
        assertEquals(99_457, earnings.totalSatoshis());
    }

    @Test
    public void differentSwapsAddUp() {
        AsbEarnings earnings = new AsbEarnings(new JSONObject());
        earnings.record("swap-a", 1_000);
        earnings.record("swap-b", 2_500);
        assertEquals(3_500, earnings.totalSatoshis());
    }

    @Test
    public void invalidRecordsAreIgnored() {
        AsbEarnings earnings = new AsbEarnings(new JSONObject());
        assertFalse(earnings.record(null, 10));
        assertFalse(earnings.record(" ", 10));
        assertFalse(earnings.record("swap-a", 0));
        assertFalse(earnings.record("swap-a", -5));
        assertEquals(0, earnings.totalSatoshis());
    }

    @Test
    public void theTotalSurvivesSerializationAcrossRestarts() {
        AsbEarnings first = new AsbEarnings(new JSONObject());
        first.record("swap-a", 4_000);
        first.markBackfilled();
        AsbEarnings second = new AsbEarnings(new JSONObject(first.toString()));
        assertEquals(4_000, second.totalSatoshis());
        assertFalse(second.needsBackfill());
        assertFalse(second.record("swap-a", 4_000));
    }

    @Test
    public void backfillParsesOnlyEarnedLinesWithAnIdAndAmount() {
        List<String> lines = List.of(
                "{\"timestamp\":\"t\",\"level\":\"INFO\",\"fields\":{\"message\":\"ASB_EARNED_BTC\",\"swap_id\":\"swap-a\",\"earned\":99457}}",
                "{\"timestamp\":\"t\",\"level\":\"INFO\",\"fields\":{\"message\":\"ASB_EARNED_BTC\",\"earned\":5}}",
                "{\"timestamp\":\"t\",\"level\":\"INFO\",\"fields\":{\"message\":\"ASB_REDEEM_TX\",\"swap_id\":\"swap-b\"}}",
                "not json ASB_EARNED_BTC");
        Map<String, AsbEarnings.Entry> found = AsbEarnings.parseEarnedLines(lines);
        assertEquals(1, found.size());
        assertEquals(99_457, found.get("swap-a").sats());
    }

    @Test
    public void backfillTakesTheXmrSoldFromTheReservationWhenTheEarnedLineHasNone() {
        List<String> lines = List.of(
                "{\"fields\":{\"message\":\"ASB_LIQUIDITY_RESERVED for incoming swap setup\",\"swap_id\":\"swap-a\",\"xmr\":\"0.123144019632 XMR\"}}",
                "{\"fields\":{\"message\":\"ASB_EARNED_BTC\",\"swap_id\":\"swap-a\",\"earned\":99457}}",
                "{\"fields\":{\"message\":\"ASB_LIQUIDITY_RERESERVED\",\"swap_id\":\"swap-b\",\"amount\":\"0.5 XMR\"}}",
                "{\"fields\":{\"message\":\"ASB_EARNED_BTC\",\"swap_id\":\"swap-b\",\"earned\":1000,\"xmr_sold_piconero\":400000000000}}");
        Map<String, AsbEarnings.Entry> found = AsbEarnings.parseEarnedLines(lines);
        assertEquals(123_144_019_632L, found.get("swap-a").xmrPico());
        // the earned line's own amount wins over the reservation
        assertEquals(400_000_000_000L, found.get("swap-b").xmrPico());
    }

    @Test
    public void xmrTotalsAddUpAndAreCountedOncePerSwap() {
        AsbEarnings earnings = new AsbEarnings(new JSONObject());
        assertTrue(earnings.record("swap-a", 1_000, 123_144_019_632L));
        assertFalse(earnings.record("swap-a", 1_000, 123_144_019_632L));
        earnings.record("swap-b", 2_000, 500_000_000_000L);
        assertEquals(3_000, earnings.totalSatoshis());
        assertEquals(623_144_019_632L, earnings.totalXmrPico());
    }

    @Test
    public void aMissingXmrAmountIsFilledInLaterWithoutCountingTheBtcTwice() {
        AsbEarnings earnings = new AsbEarnings(new JSONObject());
        earnings.record("swap-a", 99_457);
        assertEquals(0, earnings.totalXmrPico());
        assertTrue(earnings.record("swap-a", 99_457, 123_144_019_632L));
        assertEquals(99_457, earnings.totalSatoshis());
        assertEquals(123_144_019_632L, earnings.totalXmrPico());
    }

    @Test
    public void theFirstVersionsBareNumberStillReads() {
        AsbEarnings earnings = new AsbEarnings(new JSONObject("{\"swaps\":{\"swap-a\":99457}}"));
        assertEquals(99_457, earnings.totalSatoshis());
        assertEquals(0, earnings.totalXmrPico());
    }

    @Test
    public void xmrIsFormattedWithoutNoise() {
        assertEquals("0.123144", AsbEarnings.formatXmr(123_144_019_632L));
        assertEquals("1.5", AsbEarnings.formatXmr(1_500_000_000_000L));
        assertEquals("0.0", AsbEarnings.formatXmr(0));
    }

    @Test
    public void aFileFromBeforeXmrWasTrackedIsBackfilledAgain() {
        AsbEarnings old = new AsbEarnings(new JSONObject("{\"backfilled\":true,\"swaps\":{\"swap-a\":99457}}"));
        assertTrue(old.needsBackfill());
        assertTrue(old.record("swap-a", 99_457, 123_144_019_632L));
        old.markBackfilled();
        assertFalse(old.needsBackfill());
        assertEquals(99_457, old.totalSatoshis());
        assertEquals(123_144_019_632L, old.totalXmrPico());
    }
}

