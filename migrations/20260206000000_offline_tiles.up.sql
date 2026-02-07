CREATE TABLE offline_tile (
    region_id INTEGER NOT NULL,

    x INTEGER NOT NULL,
    y INTEGER NOT NULL,
    z INTEGER NOT NULL,

    UNIQUE (region_id, x, y, z)
);

CREATE INDEX offline_tile_region_id_index ON offline_tile (region_id);
CREATE INDEX offline_tile_x_y_z_index ON offline_tile (x, y, z);
CREATE INDEX tile_x_y_z_index ON tile (x, y, z);
