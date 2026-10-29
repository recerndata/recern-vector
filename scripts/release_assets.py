"""Package only verified build outputs; generate portable SHA256SUMS."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import tomllib
import zipfile

root=Path(__file__).resolve().parents[1]
version=tomllib.loads((root/'Cargo.toml').read_text())['workspace']['package']['version']
p=argparse.ArgumentParser();p.add_argument('mode',choices=['native','checksums']);p.add_argument('--platform');p.add_argument('--out',default='dist');a=p.parse_args()
out=Path(a.out).resolve();out.mkdir(parents=True,exist_ok=True)
if a.mode=='checksums':
 files=sorted(f for f in out.iterdir() if f.is_file() and f.name!='SHA256SUMS')
 if not files:raise SystemExit('no release files')
 (out/'SHA256SUMS').write_text(''.join(hashlib.sha256(f.read_bytes()).hexdigest()+'  '+f.name+'\n' for f in files),encoding='utf8')
 print('checksummed',len(files),'files')
else:
 if a.platform not in ['linux-x64','linux-arm64','darwin-x64','darwin-arm64','win32-x64']:raise SystemExit('unsupported platform')
 windows=a.platform.startswith('win32');suffix='.exe' if windows else ''
 folder=out/f'recern-vector-{version}-{a.platform}';folder.mkdir()
 (folder/'bin').mkdir();(folder/'api').mkdir()
 for name in ['recern-vector','recern-vector-server']:
  shutil.copy2(root/'target/release'/(name+suffix),folder/'bin'/(name+suffix))
 for name in ['LICENSE-MIT','LICENSE-APACHE']:
  shutil.copy2(root/name,folder/name)
 shutil.copy2(root/'api/openapi.json',folder/'api/openapi.json')
 (folder/'README.txt').write_text(f'Recern Vector {version}\nPlatform: {a.platform}\n\nCLI: bin/recern-vector{suffix} --help\nServer: bin/recern-vector-server{suffix} --help\n\nThe HTTP server requires a new/empty server-owned database and a private API-key file.\nUse verified HTTPS through a reverse proxy outside loopback. The database fits in RAM; save encoding remains O(database size).\n\nServer guide: https://recern.net/vector/docs/server\nPortable clients: https://recern.net/vector/docs/portable-clients\nRelease notes: https://github.com/recerndata/recern-vector/releases/tag/v{version}\n',encoding='utf8')
 if windows:
  with zipfile.ZipFile(out/(folder.name+'.zip'),'w',zipfile.ZIP_DEFLATED) as archive:
   for f in folder.rglob('*'):
    if f.is_file():archive.write(f,f.relative_to(out))
 else:
  with tarfile.open(out/(folder.name+'.tar.gz'),'w:gz') as archive:archive.add(folder,arcname=folder.name)
 shutil.rmtree(folder)
 print('packaged',a.platform)
