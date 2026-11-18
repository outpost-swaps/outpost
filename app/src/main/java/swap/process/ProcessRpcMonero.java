package swap.process;

public record ProcessRpcMonero(Process process, String endpoint) {
    public static boolean shutdown(String pid) {
        System.out.println("Shutting down Monero Wallet RPC with PID: " + pid);
        return ProcessGraceful.shutdownPid(pid, "monero-wallet-rpc");
    }

    public boolean shutdown() {
        if (process != null) {
            System.out.println("Shutting down Monero Wallet RPC: " + endpoint);
            return ProcessGraceful.shutdown(process);
        }

        return false;
    }
}
