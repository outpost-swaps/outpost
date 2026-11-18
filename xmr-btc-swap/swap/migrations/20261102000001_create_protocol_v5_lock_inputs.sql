CREATE TABLE IF NOT EXISTS protocol_v5_lock_inputs
(
    reservation_id TEXT NOT NULL,
    swap_id        TEXT,
    outpoint       TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (outpoint)
);

CREATE INDEX IF NOT EXISTS protocol_v5_lock_inputs_swap_id
    ON protocol_v5_lock_inputs (swap_id);
