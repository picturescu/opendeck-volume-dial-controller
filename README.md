# OpenDeck Volume Dial Controller

OpenDeck Volume Dial Controller is a Linux plugin for controlling individual application groups and PulseAudio/PipeWire-Pulse devices from Stream Deck keypad buttons and encoders. It retains the upstream automatic three-key mixer and adds manually assigned application dials, dynamic selector groups, device dials, application columns, Linux desktop icons, and responsive feedback.

## Screenshots

Screenshots are being prepared. See [the screenshot guide](docs/screenshots/README.md) for the views planned for this project.

## Status and platform

Release 1.2.0 is prepared for OpenDeck on Linux. The supplied package contains a dynamically linked `x86_64-unknown-linux-gnu` binary. Windows and macOS are not supported.

This is an independently maintained fork of `mdvictor/opendeck-volume-controller`. It uses the same plugin UUID and therefore replaces, rather than coexists with, the upstream plugin.

## Features

- Per-application and ordered application-group volume control.
- Sticky application selection with automatic failover when a stream disappears.
- Output sink and input source volume and mute control.
- Correct PulseAudio percentage conversion: normal volume is 100%, while 150% is optional.
- Physical dial rotation, dial press, and touchscreen tap interactions.
- XDG desktop-entry, KDE icon-theme, SVG/PNG, and Steam application-cache icon discovery.
- Cached icons, optimistic feedback, coalesced encoder commands, duplicate-frame suppression, and stale-generation rejection.
- Targeted volume/mute updates that avoid unrelated redraws and full topology refreshes.

Applications generally become selectable only after creating an active playback stream. A running but silent application may not appear. Browser streams may exist only while a tab is playing sound.

## Actions

### Volume Control Auto Grid

The retained upstream action automatically assigns detected applications to three-button vertical mixer columns. The top key shows the application and toggles mute; the upper and lower bar keys increase and decrease volume. Long-pressing the top key adds the application to the ignored list.

### Application Volume Column

Manually assigns one application or an ordered application group to the original three-key vertical layout. The top key uses the active application icon and toggles group mute. The middle and bottom keys form a continuous volume bar and adjust every stream belonging to the sticky active application.

Place this action on rows 0, 1, and 2 of one physical column. It requires a keypad model with at least three rows and is not intended for the two-row Stream Deck Plus keypad.

### Application Volume Dial

Controls one ordered application group from an encoder:

- Rotate to adjust all streams of the active application.
- Press the physical dial or tap its touchscreen segment to toggle mute.
- Choose a 100% or 150% maximum and a 1–10% step.
- Set an optional custom title.
- Keep the current application active until its audio stream disappears, then fail over by configured order.

Rapid movement uses optimistic feedback and coalesced latest-value commands.

### Dynamic Application Volume Dial

One reusable dial follows the Application Selector Button last pressed for the same device and focus group. It retains sticky arbitration inside the selected group, rejects stale focus and volume commands, and updates its title and icon when focus or the active application changes. Rotation controls volume; physical press and touchscreen tap control mute.

Known dynamic-dial stalls caused by re-entrant optimistic-state locking were fixed and exercised by repeated concurrency tests. This does not imply that every possible external audio or OpenDeck failure is impossible.

### Application Selector Button

Configures one ordered application group using the same searchable selector as Application Volume Dial. It supports saved unavailable targets, a custom title, dynamic application icons, and a focused indication. Pressing it assigns its group to the linked Dynamic Application Volume Dial; it does not alter volume or mute directly.

### Audio Device Volume Dial

Controls one stable PulseAudio/PipeWire-Pulse output sink or input source. Rotation changes volume and physical press or touchscreen tap toggles the appropriate sink/source mute API. Output-monitor sources are filtered from the normal microphone list. A 100% or 150% maximum can be selected where supported.

## Changes from the upstream plugin

Retained upstream behavior:

- Volume Control Auto Grid.
- Three-button vertical mixer layout and application mute/volume buttons.
- PulseAudio integration and PipeWire through `pipewire-pulse`.
- Ignored-application settings and compatibility with existing Auto Grid profiles.

Added by this fork:

- Application Volume Dial with ordered groups, sticky failover, custom titles, maximum-volume settings, and touchscreen feedback.
- Dynamic Application Volume Dial and Application Selector Buttons with persistent per-device focus groups.
- Audio Device Volume Dial for output sinks and filtered input sources.
- Application Volume Column for manually configured three-key groups.
- Shared XDG/KDE desktop icon resolution and generic Steam App ID/cache discovery without application-specific hardcoding.
- Correct 65,536-as-100% PulseAudio normalization.
- Targeted subscription updates, cached icon reuse, serialized feedback, optimistic rendering, bounded latest-volume commands, and fixes for stale feedback and the known dynamic-dial freeze.

## Requirements

- OpenDeck on Linux.
- PulseAudio, or PipeWire with the `pipewire-pulse` compatibility server.
- Linux x86_64 for the supplied 1.2.0 package.
- A Stream Deck with encoders for dial actions.
- A keypad with at least three rows for Auto Grid or Application Volume Column.
- Runtime libraries listed by `ldd`, notably glibc, PulseAudio, and common system libraries.

The plugin discovers desktop and Steam icons at runtime. It does not bundle Spotify, Firefox, Discord, Steam, game, or other third-party application icons.

## Installation from a GitHub Release

1. Back up your OpenDeck profile.
2. Download `opendeck-volume-dial-controller-v1.2.0-linux-x86_64.zip` and its checksum.
3. Verify it:

   ```bash
   sha256sum -c opendeck-volume-dial-controller-v1.2.0-linux-x86_64.zip.sha256
   ```

4. Extract the archive.
5. Copy its `com.victormarin.volume-controller.sdPlugin` directory into:

   ```text
   ~/.config/opendeck/plugins/
   ```

6. Restart OpenDeck.

The optional `scripts/install-local.sh` helper performs a backup before installing, but manual installation is the primary supported method.

## Updating an existing upstream installation

This fork deliberately retains `com.victormarin.volume-controller` so existing upstream Auto Grid configurations remain compatible. Consequently, the fork and upstream plugin cannot coexist in one OpenDeck installation.

Back up your OpenDeck profile and existing plugin directory before replacing it. The installer creates a timestamped sibling backup automatically.

## Configuration examples

### Dedicated dials

- Dial 1: Firefox, then Chromium.
- Dial 2: Spotify.
- Dial 3: Discord.
- Dial 4: selected Steam games, such as Counter-Strike 2 and another installed game.

The examples are ordinary application groups, not bundled application integrations.

### Device dials

- Output dial: an Elgato XLR output, headphones, speakers, or another sink.
- Input dial: an Elgato XLR microphone, USB microphone, or another non-monitor source.

## Dynamic dial workflow

Example selector groups:

- Browser: Firefox, Chromium.
- Music: Spotify.
- Comms: Discord.
- Games: selected Steam games.

Add one Dynamic Application Volume Dial and one or more Application Selector Buttons on the same Stream Deck. Configure each button, press a button to focus its group, rotate the dynamic dial for volume, and press or tap the dial for mute. The current application remains sticky until its playback stream disappears.

## Building from source

Install a current stable Rust toolchain, development headers for PulseAudio, Node.js for property-inspector validation, Python 3, and `unzip`.

On Debian or Ubuntu:

```bash
sudo apt-get install libpulse-dev nodejs python3 unzip
cargo build --release
```

Run the complete local checks:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
python3 -m json.tool manifest.json >/dev/null
git diff --check
```

## Manual installation from source

Build a validated package:

```bash
./scripts/package-release.sh
```

Then extract and copy the generated ZIP as described above, or use:

```bash
./scripts/install-local.sh dist/opendeck-volume-dial-controller-v1.2.0-linux-x86_64.zip
```

The executable inside the plugin must retain the exact manifest `CodePathLin` filename: `oa-volume-controller-x86_64-unknown-linux-gnu`.

## Troubleshooting

- No applications listed: start playback in the application, then refresh the inspector.
- Browser missing: play audible media in a tab so the browser creates a playback stream.
- Device missing: confirm it appears in `pactl list sinks short` or `pactl list sources short`.
- Input list contains no output monitors by design; select the real microphone source.
- Dynamic dial says “Select app”: configure and press an Application Selector Button on the same device and focus group.
- Plugin does not start: inspect OpenDeck’s plugin log and verify the binary name, executable bit, and `ldd` dependencies.
- Wrong or generic icon: some applications expose incomplete metadata, and some Wayland window icons are unavailable to the plugin.

When reporting logs, remove private paths, application data, and unrelated environment information.

## Known limitations

- Linux only; the provided package is x86_64.
- Depends on a reachable PulseAudio-compatible server.
- Only active playback streams are controllable as applications.
- Steam icon availability depends on local desktop entries, caches, and metadata; not every game is guaranteed to resolve.
- Native property-inspector controls follow browser/OpenDeck capabilities.
- A blocked underlying synchronous system call cannot always be cancelled, although topology work is isolated from action callbacks and guarded by timeouts.
- Auto Grid and Application Volume Column require at least three keypad rows.
- PulseAudio access uses the MIT/Apache-2.0 `libpulse-binding` crate; the
  dependency audit is recorded in [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md). Preserve action and plugin UUIDs unless a deliberately incompatible migration has been designed. Do not commit third-party application icons, private logs, tokens, profiles, or machine-specific configuration.

## Attribution

This project is an independently maintained fork of Victor Marin’s MIT-licensed [OpenDeck Volume Controller](https://github.com/mdvictor/opendeck-volume-controller). It is not endorsed by the upstream author. See [ATTRIBUTION.md](ATTRIBUTION.md).

## License

MIT. The upstream copyright notice is retained and the fork’s new work is separately attributed in [LICENSE](LICENSE).
