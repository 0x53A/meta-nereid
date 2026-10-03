"""Activity journal and deterministic running metrics. No hardware dependencies."""
import datetime
import json
import math
import os
from pathlib import Path
import time
import uuid


def clock():
    return time.clock_gettime(time.CLOCK_BOOTTIME)


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def metres(a, b):
    lat1, lat2 = math.radians(a[0]), math.radians(b[0])
    dlat = lat2 - lat1
    dlon = math.radians(((b[1] - a[1] + 180) % 360) - 180)
    h = math.sin(dlat/2)**2 + math.cos(lat1)*math.cos(lat2)*math.sin(dlon/2)**2
    return 6371008.8 * 2 * math.asin(math.sqrt(min(1, max(0, h))))


class Activity:
    def __init__(self, root, now=clock, boot=None):
        self.now = now
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(self.root, 0o700)
        self.id = str(uuid.uuid4())
        self.path = self.root / (self.id + '.jsonl')
        self.file = self.path.open('x', buffering=1)
        os.chmod(self.path, 0o600)
        self.state = 'prepared'
        self.started = None
        self.ended = None
        self.active_since = None
        self.active_seconds = 0
        self.distance = 0.0
        self.previous = None
        self.last_fix = None
        self.last_source = None
        self.track = []
        self.segment = 0
        self.sequence = 0
        self.gps_error = ''
        self.capture = None
        self.pace = None
        self.write('prepare', profile='running', boot_id=boot or Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                   schema=1, activity_id=self.id, clock='CLOCK_BOOTTIME', durable=True)
        descriptor = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    def write(self, event, durable=False, **data):
        self.sequence += 1
        record = dict(event=event, sequence=self.sequence, boottime_seconds=self.now(), utc=utc(), **data)
        self.file.write(json.dumps(record, allow_nan=False, separators=(',', ':')) + '\n')
        self.file.flush()
        if durable:
            os.fsync(self.file.fileno())
        return record

    def command(self, command, finalize=True):
        allowed = {'start': ('prepared',), 'pause': ('running',), 'resume': ('paused',),
                   'stop': ('running', 'paused', 'prepared')}
        if self.state not in allowed.get(command, ()):
            raise ValueError('Invalid activity transition')
        now = self.now()
        self.write(command, durable=True)
        if command == 'start':
            self.started = now
            self.active_since = now
            self.state = 'running'
        elif command == 'pause':
            self.active_seconds += now - self.active_since
            self.active_since = None
            self.state = 'paused'
        elif command == 'resume':
            self.active_since = now
            self.state = 'running'
        else:
            if self.active_since is not None:
                self.active_seconds += now - self.active_since
            self.active_since = None
            self.ended = now
            self.state = 'stopped'
        self.segment += 1
        self.previous = None
        self.pace = None
        if command == 'stop' and finalize:
            self.finalize()

    def finalize(self):
        self.write('summary', durable=True, summary=self.snapshot())
        self.file.close()

    def interrupt(self, reason):
        if self.state in ('stopped', 'interrupted'):
            return
        now = self.now()
        if self.active_since is not None:
            self.active_seconds += now - self.active_since
        self.active_since = None
        self.ended = now
        self.state = 'interrupted'
        self.write('interrupted', durable=True, reason=reason)
        self.file.close()

    def reference(self, status):
        capture = status.get('capture')
        if capture and capture != self.capture:
            self.write('sensor_capture', durable=True, path=capture, revision=status.get('revision'))
            self.capture = capture

    def gps(self, record):
        self.write('gps', raw=record)
        if self.state in ('stopped', 'interrupted'):
            return
        if record.get('event') == 'session_end':
            self.gps_error = record.get('error') or 'GPS recording ended'
            self.previous = None
            self.last_fix = None
            return
        if not record.get('interface', '').endswith('.Position'):
            return
        args = record.get('arguments', [])
        good = record.get('fresh_for_session') is True and len(args) == 6
        if not good:
            self.previous = None
            return
        try:
            fields, source, lat, lon, altitude, accuracy = args
            valid_number = lambda n: isinstance(n, (float, int)) and not isinstance(n, bool) and math.isfinite(n)
            good = (all(valid_number(n) for n in (source, lat, lon)) and abs(lat) <= 90 and abs(lon) <= 180
                    and fields & 3 == 3 and source > 0)
            acc = accuracy[1] if len(accuracy) == 3 else None
            # GeoClue accuracy level 6 is detailed. Unknown accuracy is retained raw only.
            good = good and accuracy[0] == 6 and valid_number(acc) and 0 < acc <= 35
        except (ValueError, TypeError, IndexError):
            good = False
        if not good:
            self.previous = None
            return
        now = self.now()
        if self.last_source is not None and source <= self.last_source:
            return
        self.last_source = source
        self.last_fix = now
        self.gps_error = ''
        if self.state != 'running':
            self.previous = None
            return
        point = dict(lat=lat, lon=lon, source_utc_s=source, boottime_seconds=now, segment=self.segment,
                     accuracy_m=acc)
        if fields & 4 and valid_number(altitude):
            point['altitude_m'] = altitude
        if self.previous:
            dt = source - self.previous['source_utc_s']
            receive_dt = now - self.previous['boottime_seconds']
            distance = metres((self.previous['lat'], self.previous['lon']), (lat, lon))
            speed = distance / dt if dt > 0 else math.inf
            if 0 < dt <= 10 and 0 <= receive_dt <= 10 and speed <= 12:
                self.distance += distance
                self.pace = 1000 / speed if speed >= 0.5 else None
            else:
                self.segment += 1
                point['segment'] = self.segment
                self.pace = None
        self.previous = point
        self.track.append(point)
        # UI is bounded; complete accepted points remain in the journal.
        if len(self.track) > 2000:
            self.track = self.track[::2]
        self.write('track_point', point=point, distance_m=self.distance)

    def snapshot(self):
        now = self.ended if self.ended is not None else self.now()
        active = self.active_seconds + (now-self.active_since if self.active_since is not None else 0)
        age = None if self.last_fix is None else max(0, now-self.last_fix)
        return dict(id=self.id, state=self.state, distance_m=self.distance, active_seconds=active,
                    elapsed_seconds=0 if self.started is None else now-self.started,
                    pace_seconds_km=self.pace if age is not None and age <= 10 and self.state == 'running' else None,
                    average_pace_seconds_km=active*1000/self.distance if self.distance >= 10 else None,
                    gps_lock=age is not None and age <= 10, gps_age_seconds=age, gps_error=self.gps_error,
                    track=self.track, journal=str(self.path), capture=self.capture)
