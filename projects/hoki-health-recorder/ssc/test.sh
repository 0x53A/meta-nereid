#!/bin/sh
set -eu
cd "$(dirname "$0")"
mkdir -p build/tests
for name in journal worker owner tracker time status wait notify discovery config controls limits extra_reads; do
 flags=
 case "$name" in
  worker) flags='-Wl,--wrap=fsync -Wl,--wrap=write -Wl,--wrap=pthread_cond_timedwait' ;;
  journal|status|config) flags=-Wl,--wrap=fsync ;;
 esac
 "${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pthread -g -fsanitize=address,undefined -I . $flags "tests/test_$name.c" -o "build/tests/$name"
 if test "$name" = tracker; then
  python3 tests/test_protocol.py build/tests/tracker
 elif test "$name" = discovery; then
  python3 tests/test_discovery.py build/tests/discovery 18
  "${CC:-cc}" -std=c11 -Wall -Wextra -Werror -pthread -g -fsanitize=address,undefined -I . -DSSC_EXTENDED_DISCOVERY tests/test_discovery.c -o build/tests/discovery-extended
  python3 tests/test_discovery.py build/tests/discovery-extended 47
 elif test "$name" = config; then
  python3 tests/test_config.py build/tests/config
 else
  "build/tests/$name"
 fi
done
