"""Prepare registry packages from verified native archives without publishing.

Release runs require all five targets. --platform is only for local host checks.
The umbrella package pins optional exact-version native dependencies.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
from release_version import workspace_version
ROOT=Path(__file__).resolve().parents[1]
TARGETS={
 'darwin-arm64':('darwin-arm64','darwin','arm64'),
 'darwin-x64':('darwin-x64','darwin','x64'),
 'linux-arm64':('linux-arm64-gnu','linux','arm64'),
 'linux-x64':('linux-x64-gnu','linux','x64'),
 'win32-x64':('win32-x64-msvc','win32','x64'),
}
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--artifacts',type=Path,required=True)
p.add_argument('--out',type=Path,required=True)
p.add_argument('--platform',action='append',choices=TARGETS,help='Local host check; omit to package every release platform')
a=p.parse_args();a.out=a.out.resolve();a.out.mkdir(parents=True,exist_ok=False)
version=workspace_version();source=ROOT/'crates/recern-vector-node'
manifest=json.loads((source/'package.json').read_text());assert manifest['version']==version
packages=[]
def pack(folder,name):
 result=json.loads(subprocess.check_output(['npm','pack','--json','--ignore-scripts','--pack-destination',str(a.out)],cwd=folder,text=True))
 archive=a.out/result[0]['filename']
 packages.append({'name':name,'version':version,'file':archive.name,'sha256':hashlib.sha256(archive.read_bytes()).hexdigest()})
for platform,(target,node_os,cpu) in TARGETS.items():
 if a.platform and platform not in a.platform:continue
 matches=list(a.artifacts.rglob(f'recern-vector-{version}-node-{platform}.tgz'))
 if len(matches)!=1:raise SystemExit(f'expected one verified Node archive for {platform}, found {len(matches)}')
 folder=a.out/target;folder.mkdir()
 with tarfile.open(matches[0]) as archive:
  original=json.load(archive.extractfile('package/package.json'))
  assert original['version']==version and original['os']==[node_os] and original['cpu']==[cpu]
  binary=archive.extractfile('package/recern_vector.node').read()
 (folder/'recern_vector.node').write_bytes(binary)
 value={'name':f'@recern/vector-{target}','version':version,'description':f'Recern Vector native addon for {target}',
  'main':'recern_vector.node','files':['recern_vector.node','LICENSE-*'],'license':manifest['license'],
  'os':[node_os],'cpu':[cpu],'engines':manifest['engines'],'repository':manifest['repository'],'publishConfig':{'access':'public'}}
 if node_os=='linux':value['libc']=['glibc']
 (folder/'package.json').write_text(json.dumps(value,indent=2)+'\n')
 for f in source.glob('LICENSE-*'):shutil.copy2(f,folder/f.name)
 pack(folder,value['name'])
folder=a.out/'main';folder.mkdir()
for name in ['index.js','index.d.ts','README.md']:shutil.copy2(source/name,folder/name)
shutil.copytree(source/'portable',folder/'portable')
for f in source.glob('LICENSE-*'):shutil.copy2(f,folder/f.name)
value=dict(manifest)
for key in ['scripts','os','cpu','libc']:value.pop(key,None)
value['publishConfig']={'access':'public'}
assert value['optionalDependencies']=={f'@recern/vector-{target}':version for target,_,_ in TARGETS.values()}
(folder/'package.json').write_text(json.dumps(value,indent=2)+'\n')
pack(folder,value['name'])
(a.out/'packages.json').write_text(json.dumps({'version':version,'packages':packages},indent=2)+'\n')
for folder in [a.out/'main',*(a.out/target for platform,(target,_,_) in TARGETS.items() if not a.platform or platform in a.platform)]:shutil.rmtree(folder)
print(f'Prepared {len(packages)} registry tarballs at {a.out}')
