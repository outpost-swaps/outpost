CREATE TABLE IF NOT EXISTS protocol_v5_swap_states
(
    swap_id    TEXT NOT NULL,
    role       TEXT NOT NULL,
    state      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (swap_id, role)
);
