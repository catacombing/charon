# Charon

## Syntax

Charon's configuration file uses the TOML format. The format's specification
can be found at _https://toml.io/en/v1.0.0_.

## Location

Charon doesn't create the configuration file for you, but it looks for one
at <br> `${XDG_CONFIG_HOME:-$HOME/.config}/charon/charon.toml`.

## Fields

### font

This section documents the `[font]` table.

|Name|Description|Type|Default|
|-|-|-|-|
|family|Font family|text|`"sans"`|
|size|Font size|float|`18.0`|

### colors

This section documents the `[color]` table.

|Name|Description|Type|Default|
|-|-|-|-|
|foreground|Primary foreground color|color|`"#ffffff"`|
|background|Primary background color|color|`"#181818"`|
|highlight|Primary accent color|color|`"#752a2a"`|
|alt_foreground|Alternative foreground color|color|`"#bfbfbf"`|
|alt_background|Alternative background color|color|`"#282828"`|

### tiles

This section documents the `[tiles]` table.

|Name|Description|Type|Default|
|-|-|-|-|
|server|Raster tile server.<br><br>This should be your tile server's URI, using the variables `{x}` and `{y}` for the tile numbers and `{z}` for the zoom level.|text|`https://tile.jawg.io/c09eed68-abaf-45b9-bed8-8bb2076013d7/{z}/{x}/{y}.png`|
|max_mem_tiles|Maximum number of map tiles cached in memory.<br><br>Tiles average ~100kB, which means 1_000 tiles will take around 100MB of RAM. A 720x1440p screen fits 18-28 tiles at a time.|integer|`1000`|
|max_fs_tiles|Maximum number of map tiles cached on disk.<br><br>Tiles take on average ~20kB per tile, which means 50_000 tiles will take around 1GB of disk space.<br><br>Tiles are cached at `${XDG_CACHE_HOME:-$HOME/.cache}/charon/tiles/`.|integer|`50000`|
|attribution|Tileserver attribution message|text|`"© JawgMaps © OpenStreetMap"`|

### input

This section documents the `[input]` table.

|Name|Description|Type|Default|
|-|-|-|-|
|velocity_interval|Milliseconds per velocity tick|integer|`30`|
|velocity_friction|Percentage of velocity retained each tick|float|`0.85`|
|max_tap_distance|Square of the maximum distance before touch input is considered a drag|float|`400.0`|
|max_multi_tap|Maximum interval between taps to be considered a double/trible-tap|integer (milliseconds)|`300`|
