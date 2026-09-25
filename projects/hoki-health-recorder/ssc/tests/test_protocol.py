#!/usr/bin/env python3
"""Exercise the ASan/UBSan-built native tracker with synthetic wire responses."""
from pathlib import Path
import subprocess
import sys
from wire_fixtures import indication,chunk,file_bytes
source=bytes(range(16));suid=(b'\x09'+source[:8]+b'\x11'+source[8:]).hex()
def run(lines,expected):
    raw='\n'.join(line.split()[2] for line in lines)+'\n'
    result=subprocess.run([sys.argv[1],suid],input=raw,text=True,capture_output=True)
    assert result.returncode==expected,(result.returncode,result.stdout,result.stderr)
    return result.stdout.strip()
f=file_bytes()
run([indication(source,chunk(f[:9],13,0,0)),indication(source,chunk(f[9:],14,1,0)),indication(source,chunk(tx=15))],0)
run([indication(b'\xff'*16,b'\x08\x01'),indication(source,chunk())],0)
for payloads in ([chunk(f,1,1,0),chunk(tx=3)],[chunk(),chunk(tx=2)],[chunk(eof=2)],[chunk()+b'\x10\x01'],[b'\x0a\x00']):
    run([indication(source,p) for p in payloads],2)
run([indication(source,chunk(f,1,1,0))],3)
run([indication(source,chunk())[:-2]],2)
print('9 native protocol cases passed')
