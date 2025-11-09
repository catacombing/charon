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

### input

This section documents the `[input]` table.

|Name|Description|Type|Default|
|-|-|-|-|
|max_tap_distance|Square of the maximum distance before touch input is considered a drag|float|`400.0`|
|velocity_interval|Milliseconds per velocity tick|integer|`30`|
|velocity_friction|Percentage of velocity retained each tick|float|`0.85`|
