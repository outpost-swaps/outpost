package swap.helper;

import org.json.JSONObject;
import swap.lib.AppSwap;

import java.io.*;
import java.nio.channels.FileChannel;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.util.HashMap;

public class HelperRawJsonDb {
    private static HelperRawJsonDb instance = null;
    public static HelperRawJsonDb getInstance() {
        if(instance == null) {
            instance = new HelperRawJsonDb();
        }

        return instance;
    }

    private HashMap<String, JSONObject> activeDbs = new HashMap<>();

    public JSONObject getDb(String name) {
        JSONObject db = activeDbs.get(name);
        File dbFile = new File(AppSwap.getSwapRootDir(), name + ".json");
        if(db == null && dbFile.exists()) {
            try {
                String jsonString = new String(Files.readAllBytes(Paths.get(dbFile.getAbsolutePath())));
                JSONObject dbObject = new JSONObject(jsonString);
                activeDbs.put(name, dbObject);
                return dbObject;
            } catch (IOException e) {
                throw new RuntimeException(e);
            }
        } else if(db == null) {
            JSONObject dbObject = new JSONObject();
            activeDbs.put(name, dbObject);
            return dbObject;
        }

        return db;
    }

    public void updateDb(String name, JSONObject dbObject) {
        activeDbs.put(name, dbObject);
        File dbFile = new File(AppSwap.getSwapRootDir(), name + ".json");
        try {
            // Write to a sibling temp file, flush to disk, then atomically replace the
            // real DB file. A kill mid-write can never truncate/corrupt swaps.json.
            Path target = dbFile.toPath();
            Path temp = Files.createTempFile(target.getParent(), name + "-", ".tmp");
            try (Writer output = new BufferedWriter(new FileWriter(temp.toFile()))) {
                output.write(dbObject.toString());
            }
            try (FileChannel channel = FileChannel.open(temp, StandardOpenOption.WRITE)) {
                channel.force(true);
            }
            try {
                Files.move(temp, target, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING);
            } catch (AtomicMoveNotSupportedException e) {
                Files.move(temp, target, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (Exception e) {
            // A swap must never proceed on the assumption that persistence succeeded
            // when it did not (e.g. missing swaps_account on resume = wrong-account risk).
            throw new RuntimeException("Failed to persist " + name + ".json", e);
        }
    }
}
