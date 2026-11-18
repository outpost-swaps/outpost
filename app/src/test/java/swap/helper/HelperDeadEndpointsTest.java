package swap.helper;

import org.junit.Test;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public class HelperDeadEndpointsTest {

    @Test
    public void samouraiRelicOnionIsDeadOnAnyPort() {
        assertTrue(HelperDeadEndpoints.isDead("tcp://explorerzydxu5e8hebwumrbxjtgfqrdxb2xen4rgm4f6rrn7jzumv4ltyad.onion:110"));
        assertTrue(HelperDeadEndpoints.isDead("tcp://explorerzydxu5e8hebwumrbxjtgfqrdxb2xen4rgm4f6rrn7jzumv4ltyad.onion:143"));
        assertTrue(HelperDeadEndpoints.isDead("http://446unwib5vc7j6rbarymta57wg3vyudrij7bllszihpztsoxlbobg3yd.onion:18089"));
    }

    @Test
    public void twoStrikeRemovalsAreDead() {
        assertTrue(HelperDeadEndpoints.isDead("http://node.supportxmr.com:18081"));
        assertTrue(HelperDeadEndpoints.isDead("http://node.supportxmr.ir:18081"));
        assertTrue(HelperDeadEndpoints.isDead("http://node.moneroworld.com:18089"));
        assertTrue(HelperDeadEndpoints.isDead("https://node.moneroworld.com:18089"));
        assertTrue(HelperDeadEndpoints.isDead("http://stagenet.community.rino.io:38081"));
        assertTrue(HelperDeadEndpoints.isDead("http://tiopyrxseconw73thwlv2pf5hebfcqxj5zdolym7z6pbq6gl4z7xz4ad.onion:18081"));
        assertTrue(HelperDeadEndpoints.isDead("http://hashvaultsvg2rinvxz7kos77hdfm6zrd5yco3tx2yh2linsmusfwyad.onion:18081"));
        assertTrue(HelperDeadEndpoints.isDead("http://ykqlrp7lumcik3ubzz3nfsahkbplfgqshavmgbxb4fauexqzat6idjad.onion:38081"));
    }

    @Test
    public void deadSchemePortComboIsDead() {
        assertTrue(HelperDeadEndpoints.isDead("ssl://blockstream.info:50002"));
    }

    @Test
    public void replacedStaleDefaultsAreDead() {
        assertTrue(HelperDeadEndpoints.isDead("ssl://kittyserver.ddnsfree.com:50002"));
        assertTrue(HelperDeadEndpoints.isDead("ssl://electrum.emzy.de:50002"));
        assertTrue(HelperDeadEndpoints.isDead("ssl://electrum.bitaroo.net:50002"));
        assertTrue(HelperDeadEndpoints.isDead("ssl://bitcoin.mullvad.net:5010"));
        assertTrue(HelperDeadEndpoints.isDead("ssl://wallet.blither.io:50002"));
        assertTrue(HelperDeadEndpoints.isDead("tcp://testnet.qtornado.com:51002"));
    }

    @Test
    public void testnet3ElectrumServersAreDead() {
        assertTrue(HelperDeadEndpoints.isDead("ssl://electrum.blockstream.info:60002"));
        assertTrue(HelperDeadEndpoints.isDead("ssl://blockstream.info:993"));
        assertTrue(HelperDeadEndpoints.isDead("tcp://testnet.aranguren.org:51001"));
        assertTrue(HelperDeadEndpoints.isDead("ssl://blackie.c3-soft.com:57006"));
        assertTrue(HelperDeadEndpoints.isDead("tcp://3tc6nefii2fwoc66dqvrwcyj64dd3r35ihgxvp4u37itsopns5fjtead.onion:50001"));
        assertTrue(HelperDeadEndpoints.isDead("tcp://gsw6sn27quwf6u3swgra6o7lrp5qau6kt3ymuyoxgkth6wntzm2bjwyd.onion:51001"));
    }

    @Test
    public void liveDefaultsAreNotDead() {
        assertFalse(HelperDeadEndpoints.isDead("ssl://electrum.blockstream.info:50002"));
        assertFalse(HelperDeadEndpoints.isDead("ssl://blockstream.info:700"));
        // Testnet4 electrum (2026-10-11 migration) must stay live — including the
        // aranguren/blackie hosts whose OTHER ports are denied above.
        assertFalse(HelperDeadEndpoints.isDead("ssl://blackie.c3-soft.com:57010"));
        assertFalse(HelperDeadEndpoints.isDead("tcp://testnet.aranguren.org:52001"));
        assertFalse(HelperDeadEndpoints.isDead("tcp://xufljktdbo542e6jyc7hat6ybnxx6km57rhmw5qpbzyducdcofyx2kqd.onion:50001"));
        assertFalse(HelperDeadEndpoints.isDead("tcp://xnkshos5cd6skc3mdzwrfj6erddo24hm4myfsb6b6qyedvroptijg6id.onion:50001"));
        assertFalse(HelperDeadEndpoints.isDead("https://nodes.hashvault.pro:18081"));
        assertFalse(HelperDeadEndpoints.isDead("http://nodex.monerujo.io:18081"));
        assertFalse(HelperDeadEndpoints.isDead("http://monerujods7mbghwe6cobdr6ujih6c22zu5rl7zshmizz2udf7v7fsad.onion:18081"));
        assertFalse(HelperDeadEndpoints.isDead("http://plowsofe6cleftfmk2raiw5h2x66atrik3nja4bfd3zrfa2hdlgworad.onion:18089"));
    }

    @Test
    public void nullAndEmptyAreNeverDead() {
        assertFalse(HelperDeadEndpoints.isDead(null));
        assertFalse(HelperDeadEndpoints.isDead(""));
    }
}
