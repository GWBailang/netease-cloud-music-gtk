# Upstream provenance

Source: https://github.com/gmg137/netease-cloud-music-api
Tag: 2.2.0; commit: 73ab46e5351fd3c31b67709ee1b4b918205b9e3e.
Copyright gmg137, GPLv3. Original source headers retained; COPYING included.

Local changes: additive download resource / playlist enumeration types and
cancellable streaming downloads in src/downloads.rs; module export and two
runtime dependencies. Existing playback and authentication interfaces retained.

Bug fix: song-detail `c` parameters are serialized as valid JSON, with numeric
64-bit song IDs and no extra escaping. This restores the duration lookup used
by downloads and full playlist enumeration.
