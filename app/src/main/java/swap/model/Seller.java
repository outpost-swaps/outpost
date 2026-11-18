package swap.model;

public record Seller(String multiaddr, double minQuantity, double maxQuantity, double price, String refundPolicy) {

    @Override
    public String toString() {
        return multiaddr() + ", " + price() + ", " + maxQuantity() + ", " + minQuantity() + ", " + refundPolicy();
    }

    public double getPrice() {
        return price;
    }
}