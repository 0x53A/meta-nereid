#!/usr/bin/env python3
"""Export retained logical running segments; tolerate only a torn final line."""
import argparse
import csv
import datetime
import json
from pathlib import Path
import xml.etree.ElementTree as ET


def records(path):
    with open(path, 'rb') as f:
        number = 0
        for line in f:
            if not line.endswith(b'\n'):
                break
            row = json.loads(line)
            number += 1
            if row['sequence'] != number:
                raise ValueError('Activity sequence gap')
            yield row


def export(source, destination):
    if Path(str(destination)+'.gpx').exists() or Path(str(destination)+'.csv').exists():
        raise FileExistsError('Export output already exists')
    rows = records(source)
    heart = None
    row_count = 0
    ns = 'http://www.topografix.com/GPX/1/1'
    ET.register_namespace('', ns)
    hrns = 'http://www.garmin.com/xmlschemas/TrackPointExtension/v1'
    ET.register_namespace('gpxtpx', hrns)
    gpx = ET.Element('{'+ns+'}gpx', version='1.1', creator='Hoki Activity')
    trk = ET.SubElement(gpx, 'trk')
    ET.SubElement(trk, 'name').text = 'Hoki running activity'
    ET.SubElement(trk, 'type').text = 'running'
    segment = None
    previous = None
    # Preserve markers and raw data in the original JSONL. GPX contains active segments only.
    with open(str(destination)+'.csv', 'x', newline='') as f:
        writer = csv.writer(f)
        writer.writerow(['boottime_seconds', 'latitude', 'longitude', 'distance_m', 'segment', 'heart_rate_bpm'])
        for row in rows:
            row_count += 1
            if row['event'] == 'heart_rate':
                heart = row['sample']
            if row['event'] != 'track_point':
                continue
            p = row['point']
            if p['segment'] != previous:
                segment = ET.SubElement(trk, 'trkseg')
                previous = p['segment']
            point = ET.SubElement(segment, 'trkpt', lat=str(p['lat']), lon=str(p['lon']))
            if 'altitude_m' in p:
                ET.SubElement(point, 'ele').text = str(p['altitude_m'])
            ET.SubElement(point, 'time').text = datetime.datetime.fromtimestamp(
                p['source_utc_s'], datetime.timezone.utc).isoformat().replace('+00:00', 'Z')
            bpm = None
            if heart and 0 <= p['boottime_seconds'] - heart['boottime_seconds'] <= 10:
                bpm = round(heart['bpm'])
                ext = ET.SubElement(point, 'extensions')
                tpx = ET.SubElement(ext, '{'+hrns+'}TrackPointExtension')
                ET.SubElement(tpx, '{'+hrns+'}hr').text = str(bpm)
            writer.writerow([p['boottime_seconds'], p['lat'], p['lon'], row['distance_m'], p['segment'], bpm])
    with open(str(destination)+'.gpx', 'xb') as f:
        ET.ElementTree(gpx).write(f, encoding='utf-8', xml_declaration=True)
    return row_count


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('journal', type=Path)
    parser.add_argument('destination', type=Path, help='Output basename, creates .gpx and .csv')
    args = parser.parse_args()
    export(args.journal, args.destination)
