# Third-party licensing notes

This project’s own source code is offered under the MIT license in `LICENSE`.
Its Rust dependency graph contains separately licensed software.

The direct GPL-3.0-or-later `pulsectl-rs` dependency inherited from upstream was
removed before release 1.2.0. PulseAudio access now uses `libpulse-binding`,
which declares `MIT OR Apache-2.0`, and `libpulse-sys`, which declares
`MIT OR Apache-2.0`.

The complete Cargo metadata audit for release 1.2.0 found:

- No GPL dependency.
- No AGPL dependency.
- No SSPL dependency.
- No dependency with missing Cargo license metadata.
- Permissive licenses including MIT, Apache-2.0, BSD, Zlib, BSL-1.0, Unicode,
  0BSD, and Unlicense alternatives.
- MPL-2.0 dependencies (`freedesktop-desktop-entry`, `option-ext`), which are
  file-level weak copyleft rather than strong copyleft.
- An optional LGPL alternative in `r-efi` metadata; the same crate offers MIT
  and Apache-2.0 alternatives.

The executable dynamically links the system PulseAudio client library
`libpulse.so.0`, which is provided by the user’s operating system and has its
own LGPL licensing terms. The library is not bundled in the plugin ZIP.

The audit can be reproduced with:

```bash
cargo tree
cargo metadata --format-version 1
```

Runtime-discovered desktop, icon-theme, and Steam application icons remain on
the user’s machine and are not included in the release package.

This document records dependency metadata and is not legal advice.
