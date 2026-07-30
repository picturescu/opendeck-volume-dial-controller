# Attribution

OpenDeck Volume Dial Controller is an independently maintained fork of:

- Project: OpenDeck Volume Controller
- Original author: Victor Marin
- Repository: https://github.com/mdvictor/opendeck-volume-controller
- License: MIT

The fork retains the upstream Volume Control Auto Grid action, its three-button vertical mixer concept, PulseAudio integration, ignored-application support, and the original plugin/action identity needed for profile compatibility.

New fork work adds application and device encoder actions, manually selected application columns, dynamic selector groups, Linux desktop and Steam icon resolution, corrected volume normalization, and feedback/concurrency improvements.

The original author’s copyright notice remains in `LICENSE`. The fork does not claim exclusive ownership of upstream code and does not imply endorsement by Victor Marin or the upstream project.

Third-party application icons are not distributed with this repository or its release package. They are discovered and rendered from the user’s installed desktop entries, icon themes, and application caches at runtime.

See `THIRD_PARTY_LICENSES.md` for the dependency-license audit. The inherited
GPL-licensed `pulsectl-rs` controller was removed before release 1.2.0 and
replaced by direct use of MIT/Apache-2.0 `libpulse-binding`.
