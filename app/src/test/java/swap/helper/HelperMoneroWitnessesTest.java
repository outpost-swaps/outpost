package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.util.List;
import java.util.Random;
import org.junit.Test;
import swap.model.XmrNode;

public class HelperMoneroWitnessesTest {
    private static XmrNode node(String name, String url) {
        return new XmrNode(name, url, url.contains(".onion") ? XmrNode.Type.ONION : XmrNode.Type.WEB_CORS, XmrNode.Network.MAINNET);
    }

    private static final XmrNode CAKE = node("Cake Wallet", "http://cake.onion:18081");
    private static final XmrNode PLOWSOF = node("plowsof.onion", "http://plowsof.onion:18089");
    private static final XmrNode MONERUJO = node("monerujo.onion", "http://monerujo.onion:18081");
    private static final XmrNode HASHVAULT = node("Hashvault", "https://nodes.hashvault.pro:18081");
    private static final XmrNode PLAIN_HTTP = node("StormyCloud", "http://xmr.stormycloud.org:18089");
    private static final List<XmrNode> CURATED = List.of(CAKE, PLOWSOF, MONERUJO, HASHVAULT, PLAIN_HTTP);

    private static HelperMoneroWitnesses.Selection pick(
            boolean mainnet, boolean optIn, String saved, boolean ownNode, boolean failover, List<XmrNode> curated) {
        return HelperMoneroWitnesses.select(mainnet, optIn, saved, ownNode, failover, curated, new Random(7));
    }

    @Test
    public void aPublicNodeOnMainnetNeedsTwoWitnesses() {
        HelperMoneroWitnesses.Selection selection = pick(true, false, CAKE.url(), false, false, CURATED);

        assertTrue(selection.required());
        assertNull(selection.problem());
        assertEquals("two are used", HelperMoneroWitnesses.COUNT, selection.count());
        assertTrue("spares follow the two", selection.urls().size() >= HelperMoneroWitnesses.COUNT);
        assertFalse("the user's own saved node is not its own witness", selection.urls().contains(CAKE.url()));
    }

    // A plain-http clearnet node is reached through a Tor exit relay, which can rewrite its answers.
    @Test
    public void aPlainHttpClearnetNodeIsNeverAWitness() {
        for (long seed = 0; seed < 25; seed++) {
            HelperMoneroWitnesses.Selection selection = HelperMoneroWitnesses.select(
                    true, false, CAKE.url(), false, false, CURATED, new Random(seed));
            assertFalse(selection.urls().contains(PLAIN_HTTP.url()));
        }
    }

    @Test
    public void theUsersOwnNodeIsTrustedAloneWhenFailoverIsOff() {
        HelperMoneroWitnesses.Selection selection = pick(true, false, "http://my.onion:18081", true, false, CURATED);

        assertFalse(selection.required());
        assertTrue(selection.urls().isEmpty());
        assertNull(selection.problem());
    }

    @Test
    public void aLocalNodeIsTrustedAloneWhenFailoverIsOff() {
        assertFalse(pick(true, false, "http://127.0.0.1:18081", false, false, CURATED).required());
        assertFalse(pick(true, false, "localhost:18081", false, false, CURATED).required());
    }

    // With failover on, the daemon that answers can be a public one even if the saved node is the
    // user's own, so the second opinion is needed again.
    @Test
    public void failoverBringsTheWitnessesBackForAnOwnNode() {
        HelperMoneroWitnesses.Selection selection = pick(true, false, "http://my.onion:18081", true, true, CURATED);

        assertTrue(selection.required());
        assertEquals("two are used", HelperMoneroWitnesses.COUNT, selection.count());
        assertTrue("spares follow the two", selection.urls().size() >= HelperMoneroWitnesses.COUNT);
    }

    @Test
    public void testNetworksSkipWitnessesUnlessOptedIn() {
        assertFalse(pick(false, false, CAKE.url(), false, false, CURATED).required());
        assertTrue(pick(false, false, CAKE.url(), false, false, CURATED).urls().isEmpty());

        HelperMoneroWitnesses.Selection optedIn = pick(false, true, CAKE.url(), false, false, CURATED);
        assertTrue(optedIn.required());
        assertEquals(HelperMoneroWitnesses.COUNT, optedIn.count());
        assertTrue(optedIn.urls().size() >= HelperMoneroWitnesses.COUNT);
    }

    @Test
    public void tooFewCandidatesRefuseTheSwapInsteadOfSkippingTheCheck() {
        HelperMoneroWitnesses.Selection selection = pick(true, false, CAKE.url(), false, false, List.of(CAKE, PLOWSOF, PLAIN_HTTP));

        assertTrue(selection.required());
        assertTrue(selection.urls().isEmpty());
        assertNotNull(selection.problem());
    }

    // monerujo.onion and a clearnet monerujo are one operator: two of them are not two opinions.
    @Test
    public void twoNodesOfOneOperatorAreNotBothChosen() {
        List<XmrNode> curated = List.of(
                node("monerujo", "https://nodex.monerujo.io:18081"),
                node("monerujo.onion", "http://monerujo.onion:18081"),
                CAKE,
                PLOWSOF);
        for (long seed = 0; seed < 25; seed++) {
            HelperMoneroWitnesses.Selection selection = HelperMoneroWitnesses.select(
                    true, false, "http://my.onion:18081", false, false, curated, new Random(seed));
            long monerujo = selection.urls().stream().filter(url -> url.contains("monerujo")).count();
            assertTrue("witnesses " + selection.urls(), monerujo <= 1);
        }
    }

    @Test
    public void aNodeOfTheSameOperatorAsTheSavedOneIsNotItsWitness() {
        List<XmrNode> curated = List.of(PLOWSOF, node("plowsof", "https://plowsof.example.org:18081"), CAKE, MONERUJO);
        for (long seed = 0; seed < 25; seed++) {
            HelperMoneroWitnesses.Selection selection = HelperMoneroWitnesses.select(
                    true, false, PLOWSOF.url(), false, false, curated, new Random(seed));
            assertFalse(selection.urls().stream().anyMatch(url -> url.contains("plowsof")));
        }
    }

    @Test
    public void theChoiceIsRandomButReproducibleForAGivenRandom() {
        List<XmrNode> many = List.of(CAKE, PLOWSOF, MONERUJO, HASHVAULT,
                node("a", "http://a.onion:1"), node("b", "http://b.onion:1"), node("c", "http://c.onion:1"));
        assertEquals(
                HelperMoneroWitnesses.select(true, false, "x", false, false, many, new Random(3)).urls(),
                HelperMoneroWitnesses.select(true, false, "x", false, false, many, new Random(3)).urls());
    }

    @Test
    public void sparesAreSentAlongUpToTheLimitAndEveryCandidateIsOfADifferentOperator() {
        java.util.ArrayList<XmrNode> many = new java.util.ArrayList<>();
        for (int i = 0; i < 12; i++) {
            many.add(node("op" + i + ".onion", "http://op" + i + ".onion:18081"));
        }
        HelperMoneroWitnesses.Selection selection = HelperMoneroWitnesses.select(
                true, false, "http://my.onion:18081", false, false, many, new Random(1));

        assertEquals(HelperMoneroWitnesses.COUNT + HelperMoneroWitnesses.SPARE, selection.urls().size());
        assertEquals(HelperMoneroWitnesses.COUNT, selection.count());
        assertEquals(selection.urls().size(), new java.util.HashSet<>(selection.urls()).size());
    }

    @Test
    public void exactlyTheNeededNumberIsEnoughButOneFewerIsRefused() {
        List<XmrNode> two = List.of(node("a.onion", "http://a.onion:18081"), node("b.onion", "http://b.onion:18081"));
        HelperMoneroWitnesses.Selection enough = HelperMoneroWitnesses.select(
                true, false, "http://my.onion:18081", false, false, two, new Random(1));
        assertNull(enough.problem());
        assertEquals(2, enough.urls().size());

        HelperMoneroWitnesses.Selection tooFew = HelperMoneroWitnesses.select(
                true, false, "http://my.onion:18081", false, false, two.subList(0, 1), new Random(1));
        assertTrue(tooFew.required());
        assertTrue(tooFew.problem() != null && !tooFew.problem().isBlank());
    }

    @Test
    public void noWitnessesMeansNoCount() {
        HelperMoneroWitnesses.Selection none = pick(false, false, CAKE.url(), false, false, CURATED);
        assertEquals(0, none.count());
    }
}
