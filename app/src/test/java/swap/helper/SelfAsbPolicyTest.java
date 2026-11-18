package swap.helper;

import org.junit.Test;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public class SelfAsbPolicyTest {
    private static final String PEER = "12D3KooWEMq58eksT5eJPookK5SGVkXaMSYuNMHdorKRoWYLyui8";
    private static final String OWN_ADDRESS = "/onion3/ubiamiotqmokfiidjingnx3cdlzyeo2tq4kbqoba5k6oqotknstfrgyd:9939";
    private static final String OWN_ASB = OWN_ADDRESS + "/p2p/" + PEER;
    private static final String OTHER_ASB = "/onion3/other:9939/p2p/12D3KooWRWKn8xgiGk5fuHVa11yvG87748jYHxvTVhhLM5VEuw5s";

    @Test
    public void identifiesOwnAsbByPeerId() {
        assertTrue(SelfAsbPolicy.isOwnAsb(OWN_ASB, PEER));
        assertFalse(SelfAsbPolicy.isOwnAsb(OTHER_ASB, PEER));
    }

    @Test
    public void rejectsOwnAsbWhenRunningAndNotAllowed() {
        assertTrue(SelfAsbPolicy.shouldReject(OWN_ASB, true, PEER, false));
        assertFalse(SelfAsbPolicy.shouldReject(OWN_ASB, true, PEER, true));
        assertFalse(SelfAsbPolicy.shouldReject(OWN_ASB, false, PEER, false));
    }

    @Test
    public void identifiesOwnAsbByExternalAddressWithoutPeerId() {
        assertTrue(SelfAsbPolicy.isOwnAsb(OWN_ADDRESS, "", OWN_ADDRESS));
        assertTrue(SelfAsbPolicy.shouldReject(OWN_ADDRESS, true, "", OWN_ADDRESS, false));
        assertFalse(SelfAsbPolicy.isOwnAsb(OTHER_ASB, "", OWN_ADDRESS));
    }

    @Test
    public void doesNotMatchOtherAsbOrMissingPeerId() {
        assertFalse(SelfAsbPolicy.shouldReject(OTHER_ASB, true, PEER, false));
        assertFalse(SelfAsbPolicy.isOwnAsb(OWN_ASB, ""));
        assertFalse(SelfAsbPolicy.isOwnAsb(OWN_ASB, null));
    }

    @Test
    public void environmentOverrideIsStrict() {
        assertTrue(SelfAsbPolicy.isAllowedValue("1"));
        assertFalse(SelfAsbPolicy.isAllowedValue("true"));
        assertFalse(SelfAsbPolicy.isAllowedValue(""));
        assertFalse(SelfAsbPolicy.isAllowedValue(null));
    }

    @Test
    public void anotherSellerIsNeverRejected() {
        assertFalse(SelfAsbPolicy.shouldReject(OTHER_ASB, true, PEER, OWN_ADDRESS, false));
    }

    // The override keeps the published onion (the buyer dials the ASB the way any buyer
    // would); a seller address without the peer id gets it completed.
    @Test
    public void ownAsbKeepsThePublishedOnionWhenTheOverrideIsOn() {
        org.junit.Assert.assertEquals(
                OWN_ASB, SelfAsbPolicy.resolveSelfAsbMultiaddr(OWN_ASB, PEER, OWN_ADDRESS, false));
        org.junit.Assert.assertEquals(
                OWN_ASB, SelfAsbPolicy.resolveSelfAsbMultiaddr(OWN_ASB, PEER, OWN_ADDRESS, true));
        org.junit.Assert.assertEquals(
                OWN_ASB, SelfAsbPolicy.resolveSelfAsbMultiaddr(OWN_ADDRESS, PEER, OWN_ADDRESS, true));
        org.junit.Assert.assertEquals(
                OTHER_ASB, SelfAsbPolicy.resolveSelfAsbMultiaddr(OTHER_ASB, PEER, OWN_ADDRESS, true));
    }

    @Test
    public void aMissingMultiaddrIsNotOwnAndNotRejected() {
        assertFalse(SelfAsbPolicy.isOwnAsb(null, PEER, OWN_ADDRESS));
        assertFalse(SelfAsbPolicy.shouldReject(null, true, PEER, OWN_ADDRESS, false));
    }
}
