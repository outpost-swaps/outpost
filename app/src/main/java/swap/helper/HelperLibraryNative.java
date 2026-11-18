package swap.helper;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.PosixFilePermission;
import java.security.DigestInputStream;
import java.security.MessageDigest;
import java.util.EnumSet;

public class HelperLibraryNative {
    private static boolean argExists(String[] args, String desiredArg) {
        for (String arg : args) {
            if (arg.equals(desiredArg))
                return true;
        }

        return false;
    }

    public static void loadLibrary() {
        try {
            load("libatomicswap");
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }

    private static void load(String name) throws Exception {
        try {
            File so = getResourceAsFile("/" + name + ".so");
            System.load(so.getAbsolutePath());
        } catch (Exception linxException) {
            try {
                File dylib = getResourceAsFile("/" + name + ".dylib");
                System.load(dylib.getAbsolutePath());
            } catch (Exception macOsException) {
                try {
                    File dll = getResourceAsFile("/" + name.replace("lib", "") + ".dll");
                    System.load(dll.getAbsolutePath());
                } catch (Exception windowsException) {
                    throw new Exception("Could not load library for Linux, macOS, or Windows!");
                }
            }
        }
    }

    private static File getResourceAsFile(String resourcePath) throws Exception {
        InputStream in = HelperLibraryNative.class.getResourceAsStream(resourcePath);
        if (in == null) {
            throw new IOException("Native library resource not found: " + resourcePath);
        }

        Path tempFile = Files.createTempFile("atomicswap-", ".tmp");
        tempFile.toFile().deleteOnExit();
        try {
            Files.setPosixFilePermissions(
                    tempFile,
                    EnumSet.of(PosixFilePermission.OWNER_READ, PosixFilePermission.OWNER_WRITE));
        } catch (UnsupportedOperationException ignored) {
            // Windows and other non-POSIX filesystems.
        }

        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        try (DigestInputStream digestIn = new DigestInputStream(in, digest);
             FileOutputStream out = new FileOutputStream(tempFile.toFile())) {
            byte[] buffer = new byte[8192];
            int bytesRead;
            while ((bytesRead = digestIn.read(buffer)) != -1) {
                out.write(buffer, 0, bytesRead);
            }
        }

        String sourceHash = toHex(digest.digest());
        String copiedHash = sha256(tempFile);
        if (!sourceHash.equals(copiedHash)) {
            Files.deleteIfExists(tempFile);
            throw new IOException("Native library temp file failed SHA-256 verification");
        }

        return tempFile.toFile();
    }

    private static String sha256(Path path) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        try (InputStream in = Files.newInputStream(path)) {
            byte[] buffer = new byte[8192];
            int bytesRead;
            while ((bytesRead = in.read(buffer)) != -1) {
                digest.update(buffer, 0, bytesRead);
            }
        }
        return toHex(digest.digest());
    }

    private static String toHex(byte[] bytes) {
        StringBuilder result = new StringBuilder(bytes.length * 2);
        for (byte value : bytes) {
            result.append(String.format("%02x", value));
        }
        return result.toString();
    }
}
