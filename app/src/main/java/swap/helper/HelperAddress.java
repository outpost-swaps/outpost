package swap.helper;

import com.samourai.wallet.segwit.bech32.Bech32UtilGeneric;
import org.bitcoinj.core.Address;
import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.script.Script;
import swap.gui.GUISwap;
import swap.model.Multiaddr;

public class HelperAddress {
    public static boolean isBtcAddrValid(String address) {
        NetworkParameters parameters = GUISwap.appSwap.getParams();
        if (address.isEmpty()) return false;
        try {
            Address.fromBase58(parameters, address);
            return true;
        } catch (Exception e0) {
            try {
                Bech32UtilGeneric.getInstance().computeScriptPubKey(address, parameters);
                return true;
            } catch (Exception e1) {
                return false;
            }
        }
    }

    public static Address getAddress(String address) {
        return getAddress(address, GUISwap.appSwap.getParams());
    }

    public static Address getAddress(String address, NetworkParameters parameters) {
        if (address.isEmpty()) return null;
        try {
            return Address.fromBase58(parameters, address); // base58 address
        } catch (Exception e0) {
            try {
                byte[] scriptPubKey = Bech32UtilGeneric.getInstance().computeScriptPubKey(address, parameters);
                return new Script(scriptPubKey).getToAddress(parameters); // p2wpkh address
            } catch (Exception e1) {
                return null;
            }
        }
    }

    public static boolean isXmrAddrValid(String address) {
        if (address == null || address.trim().isEmpty()) return false;
        return isValidXmrAddress(address);
    }

    private static native boolean isValidXmrAddress(String address);

    public static Multiaddr parseMultiaddr(String multiaddr) {
        if (isLibp2pPeerValid(multiaddr)) {
            return parseMultiaddrUnchecked(multiaddr);
        } else {
            throw new RuntimeException("Invalid libp2p address");
        }
    }

    /** Pure string parsing without the native validity check (unit-testable). */
    public static Multiaddr parseMultiaddrUnchecked(String multiaddr) {
        String[] parts = multiaddr.split("/");
        Multiaddr.Protocol addressProtocol = Multiaddr.Protocol.valueOf(parts[1].toUpperCase());
        String address = parts[2];
        Multiaddr.Protocol netProtocol;
        int port;
        Multiaddr.Protocol webSocketProtocol = null;
        Multiaddr.Protocol peerIdProtocol;
        if (addressProtocol == Multiaddr.Protocol.ONION3) {
            String[] addressParts = address.split(":");
            String host = addressParts[0];
            if (host.endsWith(".onion")) {
                host = host.substring(0, host.length() - ".onion".length());
            }
            address = host + ".onion";
            netProtocol = Multiaddr.Protocol.TCP;
            port = Integer.parseInt(addressParts[1]);
            peerIdProtocol = Multiaddr.Protocol.valueOf(parts[3].toUpperCase());
        } else {
            netProtocol = Multiaddr.Protocol.valueOf(parts[3].toUpperCase());
            port = Integer.parseInt(parts[4]);
            int nextPart = 5;
            if (parts.length > nextPart + 1 && parts[nextPart].equalsIgnoreCase("wss")) {
                webSocketProtocol = Multiaddr.Protocol.WSS;
                nextPart++;
            }
            peerIdProtocol = Multiaddr.Protocol.valueOf(parts[nextPart].toUpperCase());
        }
        String peerId = parts[parts.length - 1];
        return new Multiaddr(addressProtocol, address, netProtocol, port, webSocketProtocol, peerIdProtocol, peerId);
    }

    public static boolean isLibp2pPeerValid(String multiaddr) {
        if (multiaddr.isEmpty()) return false;
        return isValidLibP2pAddress(multiaddr);
    }

    private static native boolean isValidLibP2pAddress(String multiaddr);
}
