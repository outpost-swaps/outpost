package swap.model;

public enum LogType {
    INFO("#33b5e5"),
    HIGHLIGHT("#ffffff"),
    WARN("#be7327"),
    ERROR("#c0201a"),
    SUCCESS("#27be4d");

    private final String colorHex;

    LogType(String color) {
        this.colorHex = color;
    }

    public String getColor() {
        return colorHex;
    }
}
