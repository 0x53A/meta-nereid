#!/usr/bin/env python3
"""Reproducible small geometric PNGs for Sidekick scenes; no external inputs."""
import math
from pathlib import Path
import struct
import zlib
ROOT = Path(__file__).resolve().parents[1] / 'assets'

def png(name, size, pixel):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    raw = b''.join(b'\0' + bytes(c for x in range(size) for c in pixel(x, y)) for y in range(size))
    content = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size,size,8,6,0,0,0))
    content += chunk(b'IDAT', zlib.compress(raw,9)) + chunk(b'IEND',b'')
    (ROOT/name).write_bytes(content)

def dial(x,y):
    dx,dy=x-47.5,y-47.5
    r=math.hypot(dx,dy)
    theta=math.atan2(dy,dx)
    mark=38<=r<=42 and abs(math.sin(theta*6))<0.16
    return (70,100,110,255) if mark else (0,0,0,255)

ROOT.mkdir(exist_ok=True)
png('dial.png',96,dial)
png('black.png',96,lambda x,y:(0,0,0,255))
png('hand.png',64,lambda x,y:(160,230,220,255) if (30<=x<=33 and 7<=y<=34) or ((x-31.5)**2+(y-31.5)**2<10) else (0,0,0,0))
png('dot.png',12,lambda x,y:(180,220,190,255) if (x-5.5)**2+(y-5.5)**2<20 else (0,0,0,0))
png('chevron.png',24,lambda x,y:(90,140,160,255) if abs(x-(5+abs(y-11.5)))<2 else (0,0,0,0))
