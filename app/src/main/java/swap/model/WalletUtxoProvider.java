package swap.model;

import com.samourai.wallet.hd.AddressType;
import com.samourai.wallet.send.UTXO;
import com.samourai.wallet.send.provider.SimpleUtxoKeyProvider;
import com.samourai.wallet.send.provider.UtxoProvider;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import com.samourai.whirlpool.client.wallet.data.wallet.WalletSupplier;
import org.bitcoinj.core.NetworkParameters;

import java.util.Collection;
import java.util.LinkedHashMap;
import java.util.LinkedList;
import java.util.List;
import java.util.Map;
import java.util.stream.Collectors;

// Cahoots (collaborative tx) wiring intentionally not ported here — unused today,
// and darkjar's Cahoots API (CahootsWallet) belongs with the MixingService work.
public class WalletUtxoProvider extends SimpleUtxoKeyProvider implements UtxoProvider {
    private final NetworkParameters params;
    private final Map<WhirlpoolAccount, List<UTXO>> utxosByAccount;
    private final WalletSupplier walletSupplier;

    public WalletUtxoProvider(NetworkParameters params, WalletSupplier walletSupplier) {
        this.params = params;
        this.walletSupplier = walletSupplier;
        utxosByAccount = new LinkedHashMap<>();

        // init wallets
        for (WhirlpoolAccount account : WhirlpoolAccount.values()) {
            utxosByAccount.put(account, new LinkedList<>());
        }
    }

    @Override
    public Collection<UTXO> getUtxos(WhirlpoolAccount account) {
        return utxosByAccount.get(account);
    }

    @Override
    public Collection<UTXO> getUtxos(WhirlpoolAccount account, AddressType addressType) {
        return utxosByAccount.get(account).stream().filter(utxo -> {
            String address = utxo.getOutpoints().iterator().next().getAddress();
            return AddressType.findByAddress(address, params) == addressType;
        }).collect(Collectors.<UTXO>toList());
    }

    @Override
    public String getChangeAddress(WhirlpoolAccount account, AddressType addressType) {
        return walletSupplier.getWallet(account, addressType).getNextChangeAddress().getAddressString(addressType);
    }
}
