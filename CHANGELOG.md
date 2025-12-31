# Changelog

All notable changes are documented in this file.
The sections should follow the order `Packaging`, `Added`, `Changed`, `Fixed` and `Removed`.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## 1.0.0

### Added

- ModemManager GPS support
- Online routing using Valhalla

### Fixed

- Superfluous trailing comma in Photon's geocoding addresses results
- Slow load for tiles older than 7 days

## 0.3.0

### Added

- Show tiles cache size in region management UI
- Online Photon geocoding
- Zoom-in using double-tap
- Map marker for search results
- Map long-press touch action for searching entities at a specific location

### Changed

- Switched to a dark theme using jawg.io
- Search is no longer cleared when changing back to the map view

### Fixed

- Tiles getting deleted before exceeding storage limit

## 0.2.0

### Added

- Offline geocoding

## 0.1.0

Initial release
