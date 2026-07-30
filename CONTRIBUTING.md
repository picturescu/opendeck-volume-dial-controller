# Contributing

Contributions to OpenDeck Volume Dial Controller are welcome.

## Development requirements

- Linux with PulseAudio or PipeWire plus `pipewire-pulse`.
- Current stable Rust and Cargo.
- PulseAudio development headers.
- OpenDeck for manual runtime testing.
- Node.js, Python 3, `zip`, and `unzip` for validation and packaging.

Build and validate:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
python3 -m json.tool manifest.json >/dev/null
git diff --check
./scripts/package-release.sh
```

Validate inline and shared property-inspector JavaScript with `node --check`, following the commands used in `.github/workflows/ci.yml`.

## Testing with OpenDeck

Use a development profile and back it up first. Install with `scripts/install-local.sh` or copy a packaged plugin directory into `~/.config/opendeck/plugins/`, then restart OpenDeck manually. Test external volume and mute changes as well as physical controls. State clearly which hardware/runtime tests were actually performed.

## Adding or changing actions

- Preserve the plugin UUID and every released action UUID.
- Treat settings changes as migrations; do not silently invalidate existing profiles.
- Register actions in `manifest.json` and Rust.
- Add and package every referenced property inspector and asset.
- Reuse application identity, arbitration, icon, audio registry, command, and rendering infrastructure.
- Add deterministic tests and update README and CHANGELOG.

## Style and safety

- Run `cargo fmt` and Clippy with warnings denied.
- Avoid locks across awaits, synchronous PulseAudio calls, filesystem work, or OpenDeck feedback.
- Keep high-frequency diagnostics debug-only.
- Do not commit secrets, private logs, OpenDeck profiles, absolute user paths, or third-party application icons.
- Runtime-discovered desktop and Steam icons must remain on the user’s machine.

Before submitting a change, inspect the complete diff and confirm `git diff --check` passes.
