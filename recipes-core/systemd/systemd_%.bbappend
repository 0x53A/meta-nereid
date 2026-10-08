# meta-asteroid removes timesyncd with an override-style :remove. A later
# :append cannot undo that, including meta-hoki's existing enable attempt.
# Disable only that exclusion; retain other upstream removals such as rfkill.
ASTEROID_SYSTEMD_TIMESYNCD_EXCLUDE:hoki = ""
PACKAGECONFIG:append:hoki = " timesyncd"
