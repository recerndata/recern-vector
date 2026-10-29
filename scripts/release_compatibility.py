"""Verify upgrade and backward file reads using two installed Python releases.

Run outside either package's source tree. All data lives in a new --work folder.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess

p=argparse.ArgumentParser()
p.add_argument('--old-python',required=True)
p.add_argument('--new-python',required=True)
p.add_argument('--work',required=True)
a=p.parse_args()
work=Path(a.work).resolve();work.mkdir(parents=True,exist_ok=False)
root=Path(__file__).resolve().parents[1]
common=r'''
import importlib.metadata,json,sys
from pathlib import Path
from recern_vector import Database
from recern_vector.portable import Client,Record,export_snapshot,import_snapshot
work=Path(sys.argv[1])
root=Path(sys.argv[2])
metadata={"u64":2**64-1,"i64":-(2**63),"float":[1.0],"integer":[1],"nested":{"country":"JP"}}
assert not Path(__import__('recern_vector').__file__).is_relative_to(root)
'''
create=r'''
assert importlib.metadata.version('recern-vector')=='0.3.0'
client=Client.embedded(work/'upgrade.rvec')
for metric in ['cosine','l2','dot']:
 for precision in ['f32','int8']:
  col=client.create_collection(metric+'-'+precision,2,metric,quantization=precision)
  col.upsert_many([Record('',[1,0],None),Record('merchant/東京\n\0',[.8,.6],metadata)])
assert (work/'upgrade.rvec.wal').exists()
export_snapshot(client,work/'export-v03')
print(json.dumps({'old_version':importlib.metadata.version('recern-vector'),'collections':len(client.list_collections())}))
'''
upgrade=r'''
assert importlib.metadata.version('recern-vector')=='0.4.0'
client=Client.embedded(work/'upgrade.rvec')
for name in client.list_collections():
 col=client.collection(name)
 assert col.count()==2
 r=col.get('merchant/東京\n\0')
 assert r.metadata==metadata and type(r.metadata['float'][0]) is float
 assert col.search([1,0],k=10,exact=True,filter={'nested.country':'JP'})[0].id=='merchant/東京\n\0'
 col.upsert('added-by-04',[1,0],metadata)
restored=Client.embedded(work/'imported.rvec')
assert import_snapshot(work/'export-v03',restored)['verified']
assert all(restored.collection(n).count()==2 for n in restored.list_collections())
export_snapshot(client,work/'export-v04')
# A prior format-1/2 archive remains unchanged by read-only opening.
import hashlib,shutil
for fixture in ['v1-0.0.2.rvec','v2-0.2.0.rvec']:
 source=root/'crates/recern-vector/tests/fixtures'/fixture
 assert source.exists()
 if source.exists():
  target=work/fixture;shutil.copyfile(source,target);before=hashlib.sha256(target.read_bytes()).hexdigest()
  if fixture=='v2-0.2.0.rvec':shutil.copyfile(source.with_suffix('.wal'),target.with_suffix('.rvec.wal'))
  Database.open_read_only(target)
  assert hashlib.sha256(target.read_bytes()).hexdigest()==before
if __import__('os').environ.get('RECERN_TEST_SERVER_BIN'):
 sys.path.insert(0,str(root/'api/tests'))
 from test_server import Server,KEY
 server=Server(work/'migrated-server')
 try:
  with Client.remote(server.url,api_key=KEY) as remote:
   assert import_snapshot(work/'export-v03',remote)['verified']
   assert remote.collection('cosine-f32').get('merchant/東京\n\0').metadata==metadata
   export_snapshot(remote,work/'export-server-v04')
 finally: server.stop()
print(json.dumps({'new_version':importlib.metadata.version('recern-vector'),'forward_and_migration':True}))
'''
backward=r'''
assert importlib.metadata.version('recern-vector')=='0.3.0'
client=Client.embedded(work/'upgrade.rvec',read_only=True)
assert all(client.collection(n).count()==3 for n in client.list_collections())
assert all(client.collection(n).get('added-by-04').metadata==metadata for n in client.list_collections())
restored=Client.embedded(work/'backward-import.rvec')
assert import_snapshot(work/'export-v04',restored)['verified']
assert all(restored.collection(n).count()==3 for n in restored.list_collections())
print(json.dumps({'old_reader_after_04_writes':True,'export_v1_backward_import':True}))
'''
for executable,phase in [(a.old_python,create),(a.new_python,upgrade),(a.old_python,backward)]:
 result=subprocess.run([str(Path(executable).absolute()),'-c',common+phase,str(work),str(root)],cwd=work,env=os.environ.copy(),text=True,capture_output=True,timeout=120)
 if result.returncode:
  raise SystemExit(result.stderr)
 print(result.stdout.strip())
