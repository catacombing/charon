# Charon

<p>
  <img src="./logo.svg" width="10%" align="left">

  Charon is a mobile-optimized Wayland maps and navigation application.

  <br clear="align"/>
</p>

<br />

## Screenshots

<p align="center">
  <img src="https://github.com/user-attachments/assets/415c7f51-c87a-43d8-a18d-956c1a89d7dd" width="30%"/>
  <img src="https://github.com/user-attachments/assets/e81b74fe-bb86-40b8-bdbc-a5d75f38c7d5" width="30%"/>
</p>

## Features

The following noteworthy features are currently supported:

 - Raster tile maps (partially offline, tiles need to be manually loaded once)
 - POI / Address search (fully offline)
 - List POIs/Addresses at location (fully offline)
 - Routing (online only)
 - ModemManager GPS integration

The following features are **not** supported yet, but are on the roadmap:

 - Bulk tile download for offline rendering
 - Offline Valhalla routing
 - Turn-by-turn navigation (voice instructions might be out of scope)

## Building from Source

Charon is compiled with cargo, which creates a binary at `target/release/charon`:

```sh
cargo build --release
```

To compile Charon, the following dependencies are required:
 - boost (compile time)
 - kyotocabinet (runtime)
 - sqlite3 (runtime)
 - marisa (runtime)

## GPS

To show GPS, it first needs to enabled either through a UI of choosing or
`mmcli`:

```
$ mmcli -m any --location-enable-gps-raw
```

The refresh rate is set by the modem, but can be increased from the default `30`
through `mmcli`:

```
# Change GPS refresh rate from 30, to 10 seconds (this will increase battery usage).
$ mmcli -m any --location-set-gps-refresh-rate 10
```

To allow your user to read the location from the modem, you also need to grant
it location permissions using polkit.

The rule to grant these permissions to users in the `catacomb` group can be
found in the [rules](./rules) directory.
