CREATE TABLE tile (
    id INTEGER NOT NULL PRIMARY KEY,

    tileserver TEXT NOT NULL,
    x INTEGER NOT NULL,
    y INTEGER NOT NULL,
    z INTEGER NOT NULL,

    data BLOB NOT NULL,

    ctime INTEGER NOT NULL DEFAULT (unixepoch()),
    atime INTEGER NOT NULL DEFAULT (unixepoch()),

    UNIQUE (tileserver, x, y, z)
);
