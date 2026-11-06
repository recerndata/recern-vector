"""Build and run a separate Cargo application using only published engine exports."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('--released', help='Published version instead of the source engine')
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix='recern-public-consumer-') as work:
    work = Path(work)
    (work / 'src').mkdir()
    shutil.copyfile(root / 'tests/compatibility/native-client.rs', work / 'src/main.rs')
    dependency = '{version = ' + json.dumps('=' + args.released) + '}' if args.released else '{path = ' + json.dumps(str(root / 'crates/recern-vector')) + '}'
    (work / 'Cargo.toml').write_text('[package]\nname = "recern-public-consumer"\nversion = "0.0.0"\nedition = "2024"\n[dependencies]\nrecern-vector = ' + dependency + '\nserde_json = "1"\n', encoding='utf-8')
    env = {**os.environ, 'CARGO_TARGET_DIR': str(root / 'target/public-consumer')}
    subprocess.run(['cargo', 'run', '--manifest-path', str(work / 'Cargo.toml'), '--', str(work / 'consumer.rvec')], check=True, env=env)
