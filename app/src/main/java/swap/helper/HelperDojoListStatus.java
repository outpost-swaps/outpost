package swap.helper;

/** The one-line note under the public-Dojo list: which list it is and whether a fresher one is on the way. */
public final class HelperDojoListStatus {
    private HelperDojoListStatus() {}

    public enum Phase { LOADING, DONE, FAILED }

    public static String text(HelperDojoDirectoryLoader.Source source, HelperDojoDirectory.Directory directory, Phase phase, String failureReason) {
        StringBuilder text = new StringBuilder();
        switch (source) {
            case LIVE -> {
                text.append("Current list from Dojo Bay, loaded just now: ").append(directory.entries().size()).append(" Dojos verified");
                if (directory.skipped() > 0) {
                    text.append(", ").append(directory.skipped()).append(" skipped because their signature did not check out");
                }
                text.append(".");
            }
            case SAVED -> text.append("Last list loaded from Dojo Bay, ").append(directory.snapshotDate()).append(".");
            case SHIPPED -> text.append("The list that shipped with the app, from ").append(directory.snapshotDate())
                    .append(". It may be out of date.");
        }
        if (source != HelperDojoDirectoryLoader.Source.LIVE) {
            switch (phase) {
                case LOADING -> text.append(" Loading the current list over Tor...");
                case FAILED -> text.append(" Could not load the current list (").append(failureReason == null ? "unknown reason" : failureReason).append(").");
                case DONE -> { }
            }
        }
        return text.toString();
    }
}
