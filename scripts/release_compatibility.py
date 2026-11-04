"""Verify upgrade and backward file reads using two installed Python releases.

Run outside either package's source tree. All data lives in a new --work folder.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
from release_version import python_version

p=argparse.ArgumentParser()
p.add_argument('--old-python',required=True)
p.add_argument('--old-version',default='0.4.0')
p.add_argument('--new-version',default=python_version())
p.add_argument('--new-python',required=True)
p.add_argument('--work',required=True)
a=p.parse_args()
work=Path(a.work).resolve();work.mkdir(parents=True,exist_ok=False)
root=Path(__file__).resolve().parents[1]
common=r'''
import importlib.metadata,json,sys
from pathlib import Path
from recern_vector import Database,CorruptDatabaseError
from recern_vector.portable import Client,Record,export_snapshot,import_snapshot
work=Path(sys.argv[1])
root=Path(sys.argv[2])
old_version,new_version=sys.argv[3:5]
metadata={"u64":2**64-1,"i64":-(2**63),"float":[1.0],"integer":[1],"nested":{"country":"JP"}}
assert not Path(__import__('recern_vector').__file__).is_relative_to(root)
'''
create=r'''
assert importlib.metadata.version('recern-vector')==old_version
client=Client.embedded(work/'upgrade.rvec')
for metric in ['cosine','l2','dot']:
 for precision in ['f32','int8']:
  col=client.create_collection(metric+'-'+precision,2,metric,quantization=precision)
  col.upsert_many([Record('',[1,0],None),Record('merchant/東京\n\0',[.8,.6],metadata)])
assert (work/'upgrade.rvec.wal').exists()
export_snapshot(client,work/'export-previous')
print(json.dumps({'old_version':importlib.metadata.version('recern-vector'),'collections':len(client.list_collections())}))
'''
upgrade=r'''
assert importlib.metadata.version('recern-vector')==new_version
client=Client.embedded(work/'upgrade.rvec')
for name in client.list_collections():
 col=client.collection(name)
 assert col.count()==2
 r=col.get('merchant/東京\n\0')
 assert r.metadata==metadata and type(r.metadata['float'][0]) is float
 assert col.search([1,0],k=10,exact=True,filter={'nested.country':'JP'})[0].id=='merchant/東京\n\0'
 col.upsert('added-by-current',[1,0],metadata)
native=Database(work/'upgrade.rvec')
native.set_checkpoint_policy(max_commits=1)
native['cosine-f32'].upsert('auto-checkpoint', [1,0], metadata)
native['l2-int8'].upsert('auto-checkpoint', [1,0], metadata)
native.save()
assert native.persistence_stats()['automatic_checkpoints']==1
assert native.persistence_stats()['wal_bytes']==24
native.set_checkpoint_policy()
native['cosine-f32'].upsert('after-checkpoint-wal', [0,1], metadata)
native.save()
assert native.persistence_stats()['wal_commits']==1
restored=Client.embedded(work/'imported.rvec')
assert import_snapshot(work/'export-previous',restored)['verified']
assert all(restored.collection(n).count()==2 for n in restored.list_collections())
export_snapshot(client,work/'export-current')
# New default and explicitly migrated format 3 are separate from the format-2
# fixture above, which must stay writable/readable by published older engines.
assert Database(work/'imported.rvec').format_version == 3
paged=work/'paged-upgrade.rvec'
shutil=__import__('shutil')
shutil.copyfile(work/'upgrade.rvec',paged)
shutil.copyfile(work/'upgrade.rvec.wal',str(paged)+'.wal')
converted=Database(paged)
assert converted.format_version == 2
backup=converted.upgrade_storage()
assert backup is not None and Database.open_read_only(backup).format_version == 2
assert converted.format_version == 3
export_snapshot(Client.embedded(paged,read_only=True),work/'export-paged')

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
   assert import_snapshot(work/'export-previous',remote)['verified']
   assert remote.collection('cosine-f32').get('merchant/東京\n\0').metadata==metadata
   export_snapshot(remote,work/'export-server-current')
 finally: server.stop()
print(json.dumps({'new_version':importlib.metadata.version('recern-vector'),'forward_and_migration':True}))
'''
backward=r'''
assert importlib.metadata.version('recern-vector')==old_version
client=Client.embedded(work/'upgrade.rvec',read_only=True)
assert all(client.collection(n).count()==(5 if n=='cosine-f32' else 4 if n=='l2-int8' else 3) for n in client.list_collections())
assert all(client.collection(n).get('added-by-current').metadata==metadata for n in client.list_collections())
restored=Client.embedded(work/'backward-import.rvec')
assert import_snapshot(work/'export-current',restored)['verified']
assert all(restored.collection(n).count()==(5 if n=='cosine-f32' else 4 if n=='l2-int8' else 3) for n in restored.list_collections())
assert client.collection('cosine-f32').get('after-checkpoint-wal').metadata==metadata
assert client.collection('l2-int8').get('auto-checkpoint').metadata==metadata

# A reader predating format 3 must fail explicitly without modifying files.
import hashlib
for name in ['imported.rvec','paged-upgrade.rvec']:
 path=work/name
 files=[path]+([Path(str(path)+'.wal')] if Path(str(path)+'.wal').exists() else [])
 before=[hashlib.sha256(f.read_bytes()).hexdigest() for f in files]
 try: Database(path)
 except CorruptDatabaseError as error: assert 'version' in str(error).lower() and '3' in str(error),str(error)
 else: raise AssertionError('old reader accepted format 3')
 assert before==[hashlib.sha256(f.read_bytes()).hexdigest() for f in files]
restored3=Client.embedded(work/'backward-paged-import.rvec')
assert import_snapshot(work/'export-paged',restored3)['verified']
assert all(restored3.collection(n).count()==(5 if n=='cosine-f32' else 4 if n=='l2-int8' else 3) for n in restored3.list_collections())
print(json.dumps({'old_reader_after_current_writes':True,'format_3_rejected_without_mutation':True,'format_3_export_backward_import':True,'export_v1_backward_import':True}))
'''
for executable,phase in [(a.old_python,create),(a.new_python,upgrade),(a.old_python,backward)]:
 result=subprocess.run([str(Path(executable).absolute()),'-c',common+phase,str(work),str(root),a.old_version,a.new_version],cwd=work,env=os.environ.copy(),text=True,capture_output=True,timeout=120)
 if result.returncode:
  raise SystemExit(result.stderr)
 print(result.stdout.strip())
