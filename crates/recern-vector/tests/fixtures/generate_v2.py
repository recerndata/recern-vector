"""Run once against the 0.2.0 Python extension; never regenerate in tests."""
from pathlib import Path
import recern_vector as rv
path = Path(__file__).with_name('v2-0.2.0.rvec')
db = rv.Database.create(path)
for encoding in ['f32', 'int8']:
    c = db.create_collection(encoding, 3, quantization=encoding)
    c.upsert('alpha', [1, 0, 0], {'lang': 'en', 'year': 2026})
    c.upsert('removed', [0, 1, 0])
    c.upsert('beta', [0.1, 0, 0.9], {'nested': {'tag': 'β'}})
    c.delete('removed')
db.checkpoint()
