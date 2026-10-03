"""Decode only durable gzip members; old raw recordings remain readable."""
import gzip
import io
from pathlib import Path


def segment_path(root, checkpoint, index):
    suffix = 'bin.gz' if checkpoint.get('compression') == 'gzip' else 'bin'
    return Path(root) / f'events-{index:06}.{suffix}'


def decoded_segment(root, checkpoint, index):
    path = segment_path(root, checkpoint, index)
    current = index == checkpoint['segment']
    compressed = checkpoint.get('compression') == 'gzip'
    physical = checkpoint['compressed_segment_bytes'] if current and compressed else path.stat().st_size
    if type(physical) is not int or physical < 0 or physical > path.stat().st_size:
        raise ValueError('invalid compressed checkpoint boundary')
    if compressed:
        with path.open('rb') as source:
            prefix = source.read(physical)
        with gzip.GzipFile(fileobj=io.BytesIO(prefix)) as stream:
            data = stream.read(64 * 1024 * 1024 + 1)
    else:
        with path.open('rb') as stream:
            data = stream.read(64 * 1024 * 1024 + 1)
    if len(data) > 64 * 1024 * 1024:
        raise ValueError('oversize decoded segment')
    if compressed and current and len(data) != checkpoint['segment_bytes']:
        raise ValueError('decoded gzip length disagrees with checkpoint')
    return data
