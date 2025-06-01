# Gamescope Recorder

This is a proof-of-concept for recording Gamescope (i.e. the game screen content in Gaming mode on the Steam Deck) while doing zero-copy encoding by grabbing DMABUFs from Gamescope and encoding them as H.264 via VAAPI without copying raw video buffers into system memory.

Libraries used:
- [cros-codecs](https://github.com/chromeos/cros-codecs) with VAAPI backend
- [pipewire-rs](https://gitlab.freedesktop.org/pipewire/pipewire-rs)

**Note**: I have patched those libraries in my forks in order to add missing features and fix some bugs. Upstreaming those patches will be followed up soon after cleaning them up.