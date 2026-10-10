"""Run after building Python, Node and the release CLI. No external services."""
from importlib.metadata import version
from pathlib import Path
import os
import subprocess
import tempfile
import recern_vector as rv

root = Path(__file__).resolve().parents[1]
cli = root / 'target/release' / ('recern-vector.exe' if os.name == 'nt' else 'recern-vector')
with tempfile.TemporaryDirectory(prefix='recern-interop-') as directory:
    path = Path(directory) / 'interop.rvec'
    db = rv.Database.create(path)
    c = db.create_collection('q', 3, quantization='int8')
    c.upsert('python', [1, 0, 0], {'source': 'python'})
    db.save()
    script = '''
    const {Database}=require(process.argv[1]);
    const db=new Database(process.argv[2]); const c=db.collection('q');
    if(c.get('python').metadata.source!=='python') throw Error('missing Python WAL');
    c.upsert('node',new Float32Array([0,1,0]),{source:'node'}); db.save();
    '''
    subprocess.run(['node', '-e', script, str(root / 'crates/recern-vector-node'), str(path)], check=True)
    result = subprocess.run([str(cli), 'query', str(path), 'q', '--vector', '[0,1,0]', '-k', '1', '--exact'], text=True, capture_output=True)
    assert result.returncode == 0, result.stderr
    assert 'node' in result.stdout, result.stdout
    assert rv.Database(path)['q'].get('node').metadata['source'] == 'node'
    subprocess.run([str(cli), 'checkpoint', str(path)], check=True)
    assert len(rv.Database.open_read_only(path)['q']) == 2
    assert version('recern-vector') == '0.2.0'
    print('PASS: Python → Node → Rust CLI → Python share int8, WAL and checkpoint (0.2.0)')
