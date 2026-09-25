import json,struct,subprocess,sys,tempfile
from pathlib import Path
from wire_fixtures import varint

def field(n,b):return varint(n*8+2)+varint(len(b))+b
def suid(b):return b'\x09'+b[:8]+b'\x11'+b[8:]
def packet(payload,source=b'\xab'*16):
 event=b'\x0d'+struct.pack('<I',768)+b'\x11'+struct.pack('<Q',123)+field(3,payload)
 body=field(1,suid(source))+field(2,event)
 return b'\x02'+struct.pack('<HH',len(body)+2,len(body))+body
def payload(ids):return field(1,b'fsl_min')+b''.join(field(2,suid(x)) for x in ids)
def run(packets):
 with tempfile.TemporaryDirectory() as directory:
  subprocess.run([sys.argv[1],directory],input=''.join(x.hex()+'\n' for x in packets),text=True,check=True)
  p=Path(directory)/'inventory.json';assert p.stat().st_mode&0o777==0o600
  result=json.loads(p.read_text());assert len(result['streams'])==int(sys.argv[2])
  names=[x['data_type'] for x in result['streams']];assert len(set(names))==len(names)
  assert {'fsl_rhr','fsl_wk'}<=set(names)
  return result,next(x for x in result['streams'] if x['data_type']=='fsl_min')
a=bytes(range(16));b=bytes(reversed(range(16)))
r,e=run([packet(payload([a]))]);assert not r['parse_error'] and e['status']=='unique' and e['suids']==[suid(a).hex()]
r,e=run([packet(payload([]))]);assert not r['parse_error'] and e['status']=='empty'
r,e=run([packet(payload([a,b]))]);assert not r['parse_error'] and e['status']=='ambiguous'
r,e=run([packet(payload([a]),b'\xcd'*16)]);assert not r['parse_error'] and e['status']=='no_response'
for packets in ([packet(payload([a,a]))],[packet(payload([a]))]*2,[packet(payload([a]))[:-1]],[packet(payload([a])+field(1,b'fsl_min'))],[packet(field(1,b'fsl_min')+field(2,b'\x09'))]):
 r,e=run(packets);assert r['parse_error']
for name in ('fsl_rhr','fsl_wk'):
 r,_=run([packet(field(1,name.encode())+field(2,suid(a)))])
 e=next(x for x in r['streams'] if x['data_type']==name)
 assert not r['parse_error'] and e['status']=='unique' and e['suids']==[suid(a).hex()]
print('11 discovery framing/identity/ambiguity cases passed; '+sys.argv[2]+' unique query names')
