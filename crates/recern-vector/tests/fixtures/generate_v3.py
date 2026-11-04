"""One-time producer using 0.5.0-dev.1; never regenerate fixtures in tests.

Run from the repository with the new Python wheel installed. Refuses to replace
an existing file. Check in the .rvec and renamed .wal together with SHA-256s.
"""
from pathlib import Path
import shutil
import recern_vector as rv

path = Path(__file__).with_name('v3-0.5.0-dev.1.rvec')
assert rv.FORMAT_VERSION == 3
assert not path.exists()
db = rv.Database.create(path)
db.set_checkpoint_policy()
for encoding in ['f32', 'int8']:
    c = db.create_collection(encoding, 3, quantization=encoding)
    c.upsert('alpha', [1, 0, 0], {'lang': 'en', 'year': 2026})
    c.upsert('removed', [0, 1, 0])
    c.upsert('beta', [0.1, 0, 0.9], {'nested': {'tag': 'β'}, 'large': 'x' * 6000})
    c.delete('removed')
db.create_collection('empty', 3, metric='dot')
db.checkpoint()
db['int8'].upsert('journal', [0, 0, 1], {'source': 'wal'})
db.save()
shutil.copyfile(str(path)+'.wal', path.with_suffix('.wal'))
