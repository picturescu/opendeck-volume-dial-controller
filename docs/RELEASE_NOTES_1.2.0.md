# OpenDeck Volume Dial Controller 1.2.0

This is the first public release of the independently maintained OpenDeck Volume Dial Controller fork. It retains the upstream automatic three-key mixer and adds application, dynamic-group, and audio-device encoder controls for OpenDeck on Linux.

## Highlights

- Application Volume Dial with ordered application groups and sticky failover.
- Dynamic Application Volume Dial controlled by Application Selector Buttons.
- Audio Device Volume Dial for output sinks and input sources.
- Manually configured Application Volume Column.
- Linux desktop-entry, KDE icon-theme, and Steam cache icon discovery.
- Correct PulseAudio 100%/150% normalization.
- Direct MIT/Apache-2.0 `libpulse-binding` backend; the inherited GPL
  `pulsectl-rs` dependency is no longer present.
- Optimistic/coalesced encoder feedback and targeted audio updates.
- Fixes for input source mute, stale feedback, property-inspector loading, and the known dynamic-dial freeze.

## Changes from upstream

The upstream Volume Control Auto Grid, three-button vertical layout, ignored applications, PulseAudio integration, and action UUID are retained. New actions add manual application groups, dynamic selector focus, device controls, and encoder feedback. Runtime icon matching uses local XDG/KDE and Steam metadata; no third-party application icons are bundled.

## Installation

1. Download `opendeck-volume-dial-controller-v1.2.0-linux-x86_64.zip`.
2. Optionally verify it with the adjacent `.sha256` file.
3. Extract the archive.
4. Copy `com.victormarin.volume-controller.sdPlugin` into `~/.config/opendeck/plugins/`.
5. Restart OpenDeck.

## Important upgrade warning

This fork retains the upstream plugin UUID to preserve existing Auto Grid profiles. It replaces the upstream plugin and cannot coexist with it in one OpenDeck installation. Back up your OpenDeck profile and existing plugin directory before upgrading.

## Requirements

- OpenDeck on Linux.
- x86_64 Linux for this binary package.
- PulseAudio or PipeWire with `pipewire-pulse`.
- A Stream Deck with encoders for dial actions.
- At least three keypad rows for Auto Grid and Application Volume Column.

Applications generally become available only after creating an active audio playback stream.

## Known limitations

- No Windows or macOS package.
- Steam/game icon results depend on installed desktop entries and Steam caches.
- Some Wayland application/window icon metadata is unavailable.
- The executable is dynamically linked and requires compatible system libraries.
- A blocked synchronous system call cannot always be cancelled, although topology operations are isolated from action callbacks and guarded by timeouts.
- The binary dynamically links the system-provided PulseAudio client library;
  it is not a static executable and does not bundle that system library.

## Checksums

The release workflow and local packaging script generate:

```text
opendeck-volume-dial-controller-v1.2.0-linux-x86_64.zip.sha256
```

Replace this section with the generated checksum when publishing the GitHub Release.
