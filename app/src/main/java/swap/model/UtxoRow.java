package swap.model;

import com.samourai.wallet.api.backend.beans.UnspentOutput;
import javafx.beans.property.SimpleBooleanProperty;
import org.bitcoinj.core.Coin;

/**
 * Row model for the withdraw UTXO selection TableView. Wraps an extlibj
 * UnspentOutput with a JavaFX boolean property for the select checkbox.
 */
public class UtxoRow {
    private final UnspentOutput utxo;
    private final SimpleBooleanProperty selected = new SimpleBooleanProperty(false);

    public UtxoRow(UnspentOutput utxo) {
        this.utxo = utxo;
    }

    public UnspentOutput getUtxo() {
        return utxo;
    }

    public SimpleBooleanProperty selectedProperty() {
        return selected;
    }

    public boolean isSelected() {
        return selected.get();
    }

    public String getTxidShort() {
        if (utxo.tx_hash == null || utxo.tx_hash.length() < 12) return utxo.tx_hash;
        return utxo.tx_hash.substring(0, 8) + "…" + utxo.tx_hash.substring(utxo.tx_hash.length() - 4);
    }

    public int getVout() {
        return utxo.tx_output_n;
    }

    public String getValueBtc() {
        return Coin.valueOf(utxo.value).toPlainString();
    }

    public long getValueSats() {
        return utxo.value;
    }

    public int getConfirmations() {
        return utxo.confirmations;
    }

    public String getAddress() {
        return utxo.addr == null ? "" : utxo.addr;
    }

    public String getTxid() {
        return utxo.tx_hash == null ? "" : utxo.tx_hash;
    }

    public String getAddressShort() {
        if (utxo.addr == null) return "";
        if (utxo.addr.length() <= 16) return utxo.addr;
        return utxo.addr.substring(0, 10) + "…" + utxo.addr.substring(utxo.addr.length() - 4);
    }
}
