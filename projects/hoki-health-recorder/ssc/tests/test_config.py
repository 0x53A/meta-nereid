import json,os,struct,subprocess,sys,tempfile
from pathlib import Path
from wire_fixtures import varint
source=bytes(range(16))
def field(n,b):return varint(n*8+2)+varint(len(b))+b
def packet(payload,src=source,event_id=776):
 event=b'\x0d'+struct.pack('<I',event_id)+b'\x11'+struct.pack('<Q',123)+field(3,payload)
 body=field(1,b'\x09'+src[:8]+b'\x11'+src[8:])+field(2,event)
 return b'\x02'+struct.pack('<HH',len(body)+2,len(body))+body
payload=b'\x08\x00\x12\x00'
def run(packets,success,fail_sync=False,mode="--tracking-config",event=776):
 with tempfile.TemporaryDirectory() as directory:
  env=os.environ.copy()
  if fail_sync:env['TEST_FSYNC_FAIL']='1'
  r=subprocess.run([sys.argv[1],directory,mode],input=''.join(x.hex()+'\n' for x in packets),text=True,env=env)
  assert r.returncode==(0 if success else 1),r.returncode
  path=Path(directory)/'config.json';assert path.exists()==success
  if success:
   data=json.loads(path.read_text());assert data['payload_hex']==payload.hex() and data['event_id']==event and data['mode']==mode
   assert path.stat().st_mode&0o777==0o600
run([packet(payload)],True)
run([packet(payload,b'\xff'*16),packet(payload)],True)
for packets in ([],[packet(payload,b'\xff'*16)],[packet(payload,event_id=876)],[packet(payload)]*2,[packet(payload)[:-1]],[packet(b'x'*4097)]):run(packets,False)
run([packet(payload)],False,True)
print('9 configuration source/completion/publication cases passed')

for mode,event in [("--chrm-config",775),("--tracker-config",1029)]:
 run([packet(payload,event_id=event)],True,mode=mode,event=event)
 for packets in ([packet(payload,event_id=776)],
                 [packet(payload,b'\xff'*16,event_id=event)],
                 [packet(payload,event_id=event)]*2,
                 [packet(b'x'*4097,event_id=event)]):
  run(packets,False,mode=mode,event=event)
 run([packet(payload,event_id=event)],False,True,mode=mode,event=event)
print('12 additional CHRM/tracker snapshot cases passed')
