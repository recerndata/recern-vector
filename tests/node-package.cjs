// Test npm's actual file allowlist/exports, not just the source checkout.
const {execFileSync}=require('node:child_process');
const fs=require('node:fs');
const os=require('node:os');
const path=require('node:path');
const assert=require('node:assert/strict');
const dir=fs.mkdtempSync(path.join(os.tmpdir(),'recern-node-package-'));
try {
  const result=JSON.parse(execFileSync('npm',['pack','--json','--pack-destination',dir],{cwd:path.resolve(__dirname,'../crates/recern-vector-node'),encoding:'utf8'}));
  const files=result[0].files.map(f=>f.path);
  for (const file of ['index.js','index.d.ts','portable/index.js','portable/index.d.ts','recern_vector.node']) assert.ok(files.includes(file),file);
  execFileSync('tar',['-xzf',path.join(dir,result[0].filename),'-C',dir]);
  const dbPath=path.join(dir,'package.rvec');
  const code=`const {Client}=require(${JSON.stringify(path.join(dir,'package/portable'))});
    (async()=>{const client=await Client.embedded(${JSON.stringify(dbPath)});
      const col=await client.createCollection('package',{dim:2});
      await col.upsert('id',[1,0],'{"u64":18446744073709551615,"float":[1.0]}');
      if((await col.count())!==1n || !(await col.get('id')).metadataJson.includes('18446744073709551615')) throw Error('package roundtrip failed');
      await client.close();})().catch(e=>{console.error(e);process.exitCode=1;});`;
  execFileSync(process.execPath,['-e',code],{stdio:'inherit'});
  console.log('Packed native and portable Node APIs verified.');
} finally { fs.rmSync(dir,{recursive:true,force:true}); }
