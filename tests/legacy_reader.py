"""Check the published 0.2 reader used by Workspace against current writes.

Current installed bindings create isolated fixtures; --reader-python must point
at a separate, published 0.2.0 installation. No user database is opened.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import recern_vector as rv

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--reader-python', type=Path, required=True)
p.add_argument('--work', type=Path, required=True)
a = p.parse_args()
a.work = a.work.resolve()
a.work.mkdir(parents=True, exist_ok=False)
root = Path(__file__).resolve().parents[1]
legacy = a.work/'legacy.rvec'
fixture = root/'crates/recern-vector/tests/fixtures/v2-0.2.0.rvec'
shutil.copyfile(fixture, legacy)
db = rv.Database(legacy)
db['f32'].upsert('current', [1,0,0], {'from':'current'})
db.save()
assert db.format_version == 2
migrated = a.work/'migrated.rvec'
shutil.copyfile(legacy, migrated)
shutil.copyfile(str(legacy)+'.wal', str(migrated)+'.wal')
backup = rv.Database(migrated).upgrade_storage()
new = rv.Database.create(a.work/'new.rvec')
new.create_collection('c',3,quantization='int8').upsert('a',[1,0,0])
new.save()
check = r'''
from importlib.metadata import version
from pathlib import Path
from hashlib import sha256
import json,sys
from recern_vector import Database,CorruptDatabaseError
work,backup=map(Path,sys.argv[1:3])
assert version('recern-vector')=='0.2.0'
for path in [work/'legacy.rvec',backup]:
 assert Database.open_read_only(path)['f32'].get('current').metadata=={'from':'current'}
for name in ['new.rvec','migrated.rvec']:
 path=work/name
 files=[path,Path(str(path)+'.wal')]
 before=[sha256(f.read_bytes()).hexdigest() for f in files]
 try: Database(path)
 except CorruptDatabaseError as e: assert 'version 3' in str(e),str(e)
 else: raise AssertionError('old reader accepted format 3')
 assert before==[sha256(f.read_bytes()).hexdigest() for f in files]
print(json.dumps({'reader':'0.2.0','format_2_and_backup_readable':True,'format_3_rejected_without_mutation':True}))
'''
subprocess.run([str(a.reader_python.absolute()),'-c',check,str(a.work),str(backup)],cwd=a.work,check=True)
