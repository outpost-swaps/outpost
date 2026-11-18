CREATE TABLE IF NOT EXISTS redeem_sweeps
(
    swap_id    TEXT PRIMARY KEY NOT NULL,
    txid       TEXT             NOT NULL,
    created_at TEXT             NOT NULL
);
