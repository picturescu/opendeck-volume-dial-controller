# Changelog

Notable changes are organized in a Keep a Changelog-style format.

## [1.2.0] - 2026-07-30

### Added

- Application Volume Dial with ordered application groups, sticky failover, custom titles, configurable steps, and 100%/150% limits.
- Dynamic Application Volume Dial and Application Selector Button focus groups.
- Audio Device Volume Dial for output sinks and input sources.
- Application Volume Column for manually assigned three-key mixer groups.
- Shared XDG desktop-entry, KDE icon-theme, SVG/PNG, and Steam cache icon resolution.
- Reproducible release packaging, conservative local installation, CI, release automation, attribution, and contribution documentation.

### Changed

- Replaced the inherited GPL-licensed `pulsectl-rs` controller with a small
  internal backend using MIT/Apache-2.0 `libpulse-binding`.
- Volume and mute subscription events update affected targets rather than rebuilding all audio targets.
- Rapid encoder movement uses optimistic feedback and latest-value command coalescing.
- Feedback updates use complete payloads, per-context serialization, duplicate suppression, and stale-generation checks.
- Runtime application icons are cached and reused during ordinary volume changes.

### Fixed

- PulseAudio normal volume `65536` now displays and writes as 100%; `98304` represents 150%.
- Input-device mute dispatches through the PulseAudio source mute API.
- Desktop/Steam application icon matching and high-resolution source selection.
- Application Selector Button property-inspector loading and shared selector behavior.
- Raw feedback placeholders and transient generic icons during rapid updates.
- Unrelated action redraws caused by ordinary volume changes.
- Stale dynamic focus and volume commands.
- Dynamic Application Volume Dial freeze caused by re-entrant optimistic-state locking.

### Compatibility

- Retains the upstream plugin UUID and Volume Control Auto Grid UUID.
- Replaces the upstream plugin and cannot be installed alongside it.
- Existing upstream Auto Grid profiles should remain compatible.
- Existing application-dial settings retain migration/default behavior.

### Known limitations

- Linux only; the supplied release artifact targets x86_64.
- Applications require active playback streams.
- Steam and Wayland icon metadata varies between applications.
- Requires PulseAudio or PipeWire with `pipewire-pulse`.
