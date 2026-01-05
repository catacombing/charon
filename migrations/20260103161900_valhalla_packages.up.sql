CREATE TABLE valhalla_packages (
    package TEXT NOT NULL,
    path TEXT NOT NULL,

    UNIQUE(package, path)
);
