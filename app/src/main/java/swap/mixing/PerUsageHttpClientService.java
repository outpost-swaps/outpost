package swap.mixing;

import com.google.common.net.HostAndPort;
import com.samourai.http.client.HttpUsage;
import com.samourai.http.client.IHttpClient;
import com.samourai.http.client.IHttpClientService;
import com.sparrowwallet.nightjar.http.JavaHttpClientService;
import org.berndpruenster.netlayer.tor.NativeTor;
import org.berndpruenster.netlayer.tor.TorCtlException;

import java.util.EnumMap;
import java.util.Map;

/**
 * IHttpClientService that routes each HttpUsage through its own Tor instance/circuit,
 * instead of JavaHttpClientService's single shared torProxy for every usage. Backs
 * MixingService's per-usage Tor isolation (see GUISwap.startWhirlpoolTorClients()).
 */
public class PerUsageHttpClientService implements IHttpClientService {
    private final Map<HttpUsage, JavaHttpClientService> perUsage = new EnumMap<>(HttpUsage.class);

    public PerUsageHttpClientService(Map<HttpUsage, NativeTor> torByUsage) throws Exception {
        for (HttpUsage usage : HttpUsage.values()) {
            NativeTor tor = torByUsage.get(usage);
            if (tor == null) {
                throw new IllegalStateException("No Tor instance for HttpUsage=" + usage);
            }
            try {
                int proxyPort = tor.getProxy("127.0.0.1").getPort();
                HostAndPort proxy = HostAndPort.fromParts("127.0.0.1", proxyPort);
                perUsage.put(usage, new JavaHttpClientService(proxy));
            } catch (TorCtlException e) {
                throw new Exception("Failed to get Tor proxy for " + usage, e);
            }
        }
    }

    @Override
    public IHttpClient getHttpClient(HttpUsage httpUsage) {
        return perUsage.get(httpUsage).getHttpClient(httpUsage);
    }

    /** JavaStompClientService needs the concrete JavaHttpClientService for its usage. */
    public JavaHttpClientService getJavaHttpClientService(HttpUsage httpUsage) {
        return perUsage.get(httpUsage);
    }

    public void shutdown() {
        for (JavaHttpClientService service : perUsage.values()) {
            service.shutdown();
        }
    }
}
