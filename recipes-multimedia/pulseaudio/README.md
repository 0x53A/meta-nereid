# Nereid audio session and AirPlay policy

Enable OpenSSL for the manually selected RAOP sink and keep PulseAudio running
when clients disconnect so the selected sink survives. Suspend-on-idle still
releases idle transports. The application owns receiver selection; automatic
RAOP discovery is not enabled here.

Hardware sink/source routing, DSP rewind/coalescing patches, ALSA compatibility,
and vendor audio-module fixes live in meta-hoki-ex.
