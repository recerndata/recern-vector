// Exercise the exact umbrella and host addon artifacts without registry access.
const {execFileSync}=require('node:child_process');
const fs=require('node:fs'),os=require('node:os'),path=require('node:path'),assert=require('node:assert/strict');
const source=path.resolve(process.argv[2]);
const manifest=JSON.parse(fs.readFileSync(path.join(source,'packages.json'),'utf8'));
const target={'darwin-arm64':'darwin-arm64','darwin-x64':'darwin-x64','linux-arm64':'linux-arm64-gnu','linux-x64':'linux-x64-gnu','win32-x64':'win32-x64-msvc'}[`${process.platform}-${process.arch}`];
const main=manifest.packages.find(p=>p.name==='@recern/vector');
const native=manifest.packages.find(p=>p.name===`@recern/vector-${target}`);
assert.ok(main && native);
const npmCli=[process.env.npm_execpath,path.join(path.dirname(process.execPath),'node_modules/npm/bin/npm-cli.js'),path.join(path.dirname(process.execPath),'../lib/node_modules/npm/bin/npm-cli.js')].find(p=>p && fs.existsSync(p));
const command=npmCli?[process.execPath,[npmCli]]:['npm',[]];
const dir=fs.mkdtempSync(path.join(os.tmpdir(),'recern-registry-'));
try{
 execFileSync(command[0],[...command[1],'install','--offline','--ignore-scripts','--no-audit','--no-fund','--prefix',dir,path.join(source,native.file),path.join(source,main.file)],{stdio:'inherit'});
 const entry=path.join(dir,'node_modules/@recern/vector');
 assert.equal(fs.existsSync(path.join(entry,'recern_vector.node')),false,'umbrella must select its optional host package');
 const code=`const assert=require('node:assert/strict');const rv=require(${JSON.stringify(entry)});
 assert.equal(rv.version(),${JSON.stringify(manifest.version)});
 const d=rv.Database.create(${JSON.stringify(path.join(dir,'native.rvec'))});const c=d.createCollection('q',{dim:2,quantization:'int8'});c.upsert('id',new Float32Array([1,0]),{from:'registry'});d.save();
 assert.equal(new rv.Database(${JSON.stringify(path.join(dir,'native.rvec'))}).collection('q').get('id').metadata.from,'registry');
 const {Client}=require(${JSON.stringify(entry+'/portable')});
 (async()=>{const client=await Client.embedded(${JSON.stringify(path.join(dir,'portable.rvec'))});const col=await client.createCollection('portable',{dim:2});await col.upsert('id',[1,0],'{"source":"registry"}');assert.equal((await col.get('id')).id,'id');await client.close()})().catch(e=>{console.error(e);process.exitCode=1});`;
 execFileSync(process.execPath,['-e',code],{stdio:'inherit'});
 console.log('Registry umbrella and platform addon installed and verified offline.');
}finally{fs.rmSync(dir,{recursive:true,force:true})}
