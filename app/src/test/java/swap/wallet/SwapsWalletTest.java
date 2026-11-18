package swap.wallet;

import com.samourai.wallet.api.backend.beans.UnspentOutput;
import com.samourai.wallet.client.BipWalletAndAddressType;
import com.samourai.wallet.hd.AddressType;
import com.samourai.wallet.hd.HD_Address;
import com.samourai.wallet.segwit.bech32.Bech32UtilGeneric;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import org.bitcoinj.core.Address;
import org.bitcoinj.core.Coin;
import org.bitcoinj.params.TestNet3Params;
import org.bouncycastle.util.encoders.Hex;
import org.junit.Test;

import java.util.ArrayList;
import java.util.List;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

/**
 * Exercises SwapsWallet.prepareWithdraw's coin selection, fee estimation, and signing
 * with no network involved (dojoUrl=null, explicit UTXOs) — this is the part of the
 * darkjar migration that's actually unit-testable without a live Dojo/coordinator.
 */
public class SwapsWalletTest {

    // Standard all-zeros BIP39 test vector - fixed, not a real wallet.
    private static final List<String> TEST_MNEMONIC = List.of(
            "abandon", "abandon", "abandon", "abandon", "abandon", "abandon",
            "abandon", "abandon", "abandon", "abandon", "abandon", "about");

    private static SwapsWallet buildTestWallet() {
        return new SwapsWallet(TEST_MNEMONIC, "", TestNet3Params.get(), null, null);
    }

    private static UnspentOutput fakeUtxo(BipWalletAndAddressType bw, int index, long valueSats, int confirmations) throws Exception {
        HD_Address hdAddress = bw.getAddressAt(0, index);
        String addressString = hdAddress.getAddressString(AddressType.SEGWIT_NATIVE);
        byte[] scriptBytes = Bech32UtilGeneric.getInstance().computeScriptPubKey(addressString, TestNet3Params.get());

        UnspentOutput utxo = new UnspentOutput();
        utxo.tx_hash = "11".repeat(32); // fake but well-formed 32-byte txid
        utxo.tx_output_n = 0;
        utxo.value = valueSats;
        utxo.script = Hex.toHexString(scriptBytes);
        utxo.addr = addressString;
        utxo.confirmations = confirmations;
        utxo.xpub = new UnspentOutput.Xpub();
        utxo.xpub.path = "0/0/" + index; // split[1]=chain (0=receive), split[2]=address index
        utxo.xpub.m = bw.getPub();
        return utxo;
    }

    private static UnspentOutput fakeUtxo(BipWalletAndAddressType bw, AddressType addressType,
                                          int index, long valueSats, int confirmations) throws Exception {
        HD_Address hdAddress = bw.getAddressAt(0, index);
        String addressString = hdAddress.getAddressString(addressType);
        Address address = Address.fromBase58(TestNet3Params.get(), addressString);

        UnspentOutput utxo = new UnspentOutput();
        utxo.tx_hash = String.format("%064x", index + 1);
        utxo.tx_output_n = 0;
        utxo.value = valueSats;
        utxo.script = Hex.toHexString(org.bitcoinj.script.ScriptBuilder.createOutputScript(address).getProgram());
        utxo.addr = addressString;
        utxo.confirmations = confirmations;
        utxo.xpub = new UnspentOutput.Xpub();
        utxo.xpub.path = "0/0/" + index;
        utxo.xpub.m = bw.getPub();
        return utxo;
    }

    @Test
    public void getAccountPubs_includesAllConfiguredDepositAddressTypes() {
        SwapsWallet wallet = buildTestWallet();
        assertEquals(3, wallet.getAccountPubs(WhirlpoolAccount.DEPOSIT).size());
        assertEquals(1, wallet.getAccountPubs(WhirlpoolAccount.POSTMIX).size());
        assertEquals(1, wallet.getAccountPubs(WhirlpoolAccount.SWAPS_DEPOSIT).size());
    }

    @Test
    public void prepareWithdraw_signsLegacyAndWrappedSegwitDepositUtxos() throws Exception {
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType legacy = wallet.getWalletSupplier()
                .getWallet(WhirlpoolAccount.DEPOSIT, AddressType.LEGACY);
        BipWalletAndAddressType wrapped = wallet.getWalletSupplier()
                .getWallet(WhirlpoolAccount.DEPOSIT, AddressType.SEGWIT_COMPAT);
        Address destination = legacy.getAddressAt(0, 99).getAddress();
        List<UnspentOutput> utxos = List.of(
                fakeUtxo(legacy, AddressType.LEGACY, 0, 200_000L, 6),
                fakeUtxo(wrapped, AddressType.SEGWIT_COMPAT, 1, 300_000L, 6));

        SwapsWallet.WithdrawPlan plan = wallet.prepareWithdraw(
                destination, Coin.valueOf(100_000L), WhirlpoolAccount.DEPOSIT, 10, utxos);

        assertNotNull(plan.signedTx);
        assertEquals(2, plan.signedTx.getInputs().size());
        assertEquals(2, plan.signedTx.getOutputs().size());
        assertTrue(plan.fee.isGreaterThan(Coin.ZERO));
    }

    @Test
    public void prepareWithdraw_sweepsSingleUtxoMinusFee() throws Exception {
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType source = wallet.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
        Address destination = source.getAddressAt(0, 99).getAddress();

        UnspentOutput utxo = fakeUtxo(source, 0, 100_000L, 6);

        SwapsWallet.WithdrawPlan plan = wallet.prepareWithdraw(
                destination, Coin.valueOf(Long.MAX_VALUE), WhirlpoolAccount.SWAPS_DEPOSIT, 10,
                List.of(utxo));

        assertNotNull(plan.signedTx);
        assertTrue("fee should be positive", plan.fee.isGreaterThan(Coin.ZERO));
        assertEquals("sweep has no change output", Coin.ZERO, plan.change);
        assertEquals(Coin.valueOf(100_000L), plan.amount.add(plan.fee));
        assertTrue("signed tx should have exactly one input", plan.signedTx.getInputs().size() == 1);
        assertTrue("signed tx should have exactly one output (sweep, no change)", plan.signedTx.getOutputs().size() == 1);
    }

    @Test
    public void prepareWithdraw_partialAmountProducesChangeOutput() throws Exception {
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType source = wallet.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
        Address destination = source.getAddressAt(0, 99).getAddress();

        UnspentOutput utxo = fakeUtxo(source, 0, 500_000L, 6);

        SwapsWallet.WithdrawPlan plan = wallet.prepareWithdraw(
                destination, Coin.valueOf(100_000L), WhirlpoolAccount.SWAPS_DEPOSIT, 10,
                List.of(utxo));

        assertEquals(Coin.valueOf(100_000L), plan.amount);
        assertTrue("should have leftover change", plan.change.isGreaterThan(Coin.ZERO));
        assertEquals(Coin.valueOf(500_000L), plan.amount.add(plan.change).add(plan.fee));
        assertEquals("destination + change outputs", 2, plan.signedTx.getOutputs().size());
    }

    @Test
    public void prepareWithdraw_selectsMultipleExplicitUtxosLinearly() throws Exception {
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType source = wallet.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
        Address destination = source.getAddressAt(0, 99).getAddress();

        List<UnspentOutput> utxos = new ArrayList<>();
        utxos.add(fakeUtxo(source, 0, 50_000L, 6));
        utxos.add(fakeUtxo(source, 1, 60_000L, 6));

        SwapsWallet.WithdrawPlan plan = wallet.prepareWithdraw(
                destination, Coin.valueOf(Long.MAX_VALUE), WhirlpoolAccount.SWAPS_DEPOSIT, 10, utxos);

        assertEquals("explicit UTXOs are all used, no selection skipped", 2, plan.inputs.size());
        assertEquals(Coin.valueOf(110_000L), plan.amount.add(plan.fee));
    }

    @Test(expected = IllegalStateException.class)
    public void prepareWithdraw_insufficientFundsThrows() throws Exception {
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType source = wallet.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
        Address destination = source.getAddressAt(0, 99).getAddress();

        UnspentOutput utxo = fakeUtxo(source, 0, 1_000L, 6);

        wallet.prepareWithdraw(destination, Coin.valueOf(1_000_000L), WhirlpoolAccount.SWAPS_DEPOSIT, 10, List.of(utxo));
    }

    @Test
    public void prepareWithdraw_withoutDojoStillWorksWithExplicitUtxos() throws Exception {
        // Regression check: prepareWithdraw used to require a configured Dojo backend even
        // when the caller already supplied explicit UTXOs (no fetch needed). Should not throw.
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType source = wallet.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
        Address destination = source.getAddressAt(0, 99).getAddress();
        UnspentOutput utxo = fakeUtxo(source, 0, 100_000L, 6);

        SwapsWallet.WithdrawPlan plan = wallet.prepareWithdraw(
                destination, Coin.valueOf(Long.MAX_VALUE), WhirlpoolAccount.SWAPS_DEPOSIT, 10, List.of(utxo));

        assertNotNull(plan);
    }

    @Test
    public void getUtxos_withoutDojoReturnsEmptyRatherThanThrowing() {
        SwapsWallet wallet = buildTestWallet();
        assertTrue(wallet.getUtxos(WhirlpoolAccount.SWAPS_DEPOSIT).isEmpty());
    }

    @Test
    public void prepareWithdraw_rejectsUtxoFromAnotherAccount() throws Exception {
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType foreign = wallet.getWalletSupplier()
                .getWallet(WhirlpoolAccount.BADBANK, AddressType.SEGWIT_NATIVE);
        BipWalletAndAddressType source = wallet.getWalletSupplier()
                .getWallet(WhirlpoolAccount.SWAPS_REFUNDS, AddressType.SEGWIT_NATIVE);
        Address destination = source.getAddressAt(0, 99).getAddress();

        UnspentOutput foreignUtxo = fakeUtxo(foreign, 0, 100_000L, 6);

        try {
            wallet.prepareWithdraw(destination, Coin.valueOf(Long.MAX_VALUE),
                    WhirlpoolAccount.SWAPS_REFUNDS, 10, List.of(foreignUtxo));
            fail("cross-account UTXO merge must be rejected");
        } catch (IllegalStateException expected) {
            assertTrue(expected.getMessage().contains("does not belong to account"));
        }
    }

    @Test
    public void prepareWithdraw_rejectsMixedOwnAndForeignUtxos() throws Exception {
        SwapsWallet wallet = buildTestWallet();
        BipWalletAndAddressType foreign = wallet.getWalletSupplier()
                .getWallet(WhirlpoolAccount.BADBANK, AddressType.SEGWIT_NATIVE);
        BipWalletAndAddressType source = wallet.getWalletSupplier()
                .getWallet(WhirlpoolAccount.SWAPS_REFUNDS, AddressType.SEGWIT_NATIVE);
        Address destination = source.getAddressAt(0, 99).getAddress();

        List<UnspentOutput> mixed = List.of(
                fakeUtxo(source, 0, 100_000L, 6),
                fakeUtxo(foreign, 1, 200_000L, 6));

        try {
            wallet.prepareWithdraw(destination, Coin.valueOf(Long.MAX_VALUE),
                    WhirlpoolAccount.SWAPS_REFUNDS, 10, mixed);
            fail("merging own + foreign UTXOs must be rejected");
        } catch (IllegalStateException expected) {
            assertTrue(expected.getMessage().contains("does not belong to account"));
        }
    }
}
