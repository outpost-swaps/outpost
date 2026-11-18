package swap.helper;

import java.util.List;
import org.json.JSONArray;

/**
 * Which Monero nodes / Electrum servers the user has marked as their own, and which node a
 * "Public node" / "Personal node" choice should select. Pure functions over the saved JSON array so
 * they can be tested without a window.
 */
public final class HelperOwnNodes {
    private HelperOwnNodes() {}

    /** Host part of a node address such as {@code http://user@host:18081}, lower-cased. */
    static String host(String url) {
        if (url == null) {
            return "";
        }
        String s = url.trim().toLowerCase();
        int scheme = s.indexOf("://");
        if (scheme >= 0) {
            s = s.substring(scheme + 3);
        }
        int slash = s.indexOf('/');
        if (slash >= 0) {
            s = s.substring(0, slash);
        }
        int at = s.lastIndexOf('@');
        if (at >= 0) {
            s = s.substring(at + 1);
        }
        if (s.startsWith("[")) {
            int close = s.indexOf(']');
            return close > 0 ? s.substring(1, close) : s;
        }
        int colon = s.indexOf(':');
        return colon >= 0 ? s.substring(0, colon) : s;
    }

    /** localhost, loopback and private-network addresses. */
    public static boolean isLocal(String url) {
        String h = host(url);
        if (h.equals("localhost") || h.equals("::1") || h.endsWith(".local")) {
            return true;
        }
        String[] p = h.split("\\.");
        if (p.length != 4) {
            return false;
        }
        try {
            int a = Integer.parseInt(p[0]);
            int b = Integer.parseInt(p[1]);
            return a == 127 || a == 10 || (a == 192 && b == 168) || (a == 172 && b >= 16 && b <= 31);
        } catch (NumberFormatException e) {
            return false;
        }
    }

    /**
     * What the Add Node box starts as: only local and private-network addresses, which are the
     * user's own machine or network. Not .onion: most public Monero nodes are onions, and a Monero
     * node marked as the user's own is trusted alone to confirm a seller's lock (no witnesses), so
     * that must be a deliberate choice (funds-risk review 2026-11-13).
     */
    public static boolean suggestOwn(String url) {
        return isLocal(url);
    }

    public static boolean contains(String savedJson, String url) {
        if (url == null) {
            return false;
        }
        for (String own : parse(savedJson)) {
            if (own.equals(url)) {
                return true;
            }
        }
        return false;
    }

    /** Own = marked by the user, or a local address (always the user's machine or network). */
    public static boolean isOwn(String savedJson, String url) {
        return contains(savedJson, url) || isLocal(url);
    }

    public static String with(String savedJson, String url, boolean own) {
        JSONArray out = new JSONArray();
        for (String existing : parse(savedJson)) {
            if (!existing.equals(url)) {
                out.put(existing);
            }
        }
        if (own) {
            out.put(url);
        }
        return out.toString();
    }

    /**
     * The node a radio choice selects, or {@code null} when there is none (for "personal node": the
     * user has not saved one, so the Add Node window should open).
     */
    public static String pick(boolean wantOwn, String lastOwn, String lastPublic, String savedJson, List<String> shown) {
        String last = wantOwn ? lastOwn : lastPublic;
        if (last != null && shown.contains(last) && isOwn(savedJson, last) == wantOwn) {
            return last;
        }
        for (String candidate : shown) {
            if (isOwn(savedJson, candidate) == wantOwn) {
                return candidate;
            }
        }
        return null;
    }

    private static List<String> parse(String savedJson) {
        java.util.ArrayList<String> out = new java.util.ArrayList<>();
        if (savedJson == null || savedJson.isBlank()) {
            return out;
        }
        try {
            JSONArray array = new JSONArray(savedJson);
            for (int i = 0; i < array.length(); i++) {
                out.add(array.getString(i));
            }
        } catch (Exception ignored) {
            // a damaged value is treated as "none marked"
        }
        return out;
    }
}
