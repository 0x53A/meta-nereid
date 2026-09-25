# Audiobook playback needs the Opus parser and an AAC decoder for m4a/m4b.
PACKAGECONFIG:append:hoki = " opusparse faad"
