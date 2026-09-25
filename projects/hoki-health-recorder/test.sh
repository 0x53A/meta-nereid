#!/bin/sh
# Host-only regression suite. Does not connect to the watch or deploy anything.
set -eu
cd "$(dirname "$0")"

cargo test
python3 -m unittest discover -s tools -p 'test_*.py'
python3 -m unittest discover -s deploy -p 'test_*.py'
# Other SSC Python scripts drive compiled fixtures and are invoked by ssc/test.sh.
python3 -m unittest discover -s ssc/tests -p 'test_heartbeat_payload.py'
python3 -m unittest discover -s ssc/tests -p 'test_minute_timestamp.py'
sh ssc/test.sh
