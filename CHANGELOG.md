# Changelog

All notable changes are documented in this file.
The sections should follow the order `Packaging`, `Added`, `Changed`, `Fixed` and `Removed`.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## Unreleased

### Added

- Scaled parent tiles are now used as placeholder when tiles cannot be loaded
- Offline tile download and storage

### Fixed

- Not starting with missing cache directory
- Crashing on startup due to improper Wayland initialization order

## 1.4.0 - 2026-01-22

### Added

- Route overview showing all maneuvers of a route

### Changed

- Default zoom on GPS location changed from level 19 to 18
- Search for the route target will now use the origin as reference point
- Increased input size for route buttons in search results

### Fixed

- Excessive polling/crashes with low ModemManager refresh rate
- Search not using latest GPS coordinate as reference
- Padding around search view status text
- Offline searches containing common elements like house numbers taking forever

## 1.3.0 - 2026-01-13

### Added

- Turn-by-turn navigation instructions on the map

### Fixed

- Offline routing
- Route rendered on top of GPS location/POIs

## 1.2.0 - 2026-01-11

### Added

- Button for switching between pedestrian/automobile routing modes
- Automatic updates for routes with GPS origin

### Changed

- Swapped search and back button in search view

## 1.1.0 - 2026-01-05

### Added

- GPS locking on GPS button triple-click in maps view
- Offline routing

### Fixed

- Unreadable attribution message with default tileset

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
