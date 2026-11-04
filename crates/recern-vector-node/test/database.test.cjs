const {test}=require('node:test');const assert=require('node:assert/strict');
const {mkdtempSync,rmSync}=require('node:fs');const {tmpdir}=require('node:os');const {join}=require('node:path');
const {Database,version,formatVersion}=require('..');
const f=(a)=>new Float32Array(a);
test('native database, quantization, filters, WAL, async buffer ownership and checkpoint',async()=>{
 const dir=mkdtempSync(join(tmpdir(),'recern-node-'));try{
  assert.equal(version(),require('../package.json').version);assert.equal(formatVersion(),3);
  const path=join(dir,'db.rvec');const db=Database.create(path);const c=db.createCollection('docs',{dim:3,metric:'cosine',quantization:'int8'});
  assert.equal(c.upsertMany([{id:'a',vector:f([1,0,0]),metadata:{lang:'en'}},{id:'b',vector:f([0,1,0]),metadata:{lang:'de'}}]),2);
  assert.equal(c.stats().vectorBytes,14);assert.equal(c.search(f([1,0,0]),1)[0].id,'a');
  const query=f([1,0,0]);const promise=c.searchAsync(query,1);query.fill(0);assert.equal((await promise)[0].id,'a');
  assert.equal(c.search(f([1,0,0]),2,{filter:{$not:{lang:'en'}}})[0].id,'b');
  assert.throws(()=>c.upsert('broken',f([1,2])),/dimensions/);assert.equal(c.stats().live,2);
  db.save();const ro=Database.openReadOnly(path);assert.equal(ro.readOnly,true);assert.equal(ro.collection('docs').get('a').metadata.lang,'en');assert.throws(()=>ro.save(),/read-only/);
  const stale=new Database(path);c.delete('b');db.save();assert.throws(()=>stale.save(),/another handle/);
  db.checkpoint();assert.equal(new Database(path).collection('docs').stats().live,1);
  const results=await Promise.all(Array.from({length:12},()=>c.searchAsync(f([1,0,0]),1)));assert.ok(results.every(h=>h[0].id==='a'));
 }finally{rmSync(dir,{recursive:true,force:true});}
});
test('batch validation is atomic and disposed collections reject operations',()=>{
 const dir=mkdtempSync(join(tmpdir(),'recern-node-'));try{const db=Database.create(join(dir,'db.rvec'));const c=db.createCollection('c',{dim:2,metric:'l2'});
 assert.throws(()=>c.upsertMany([{id:'a',vector:f([1,2])},{id:'b',vector:f([1])}]),/dimensions/);assert.equal(c.stats().live,0);
 db.dropCollection('c');assert.throws(()=>c.get('a'),/not found/);
 }finally{rmSync(dir,{recursive:true,force:true});}
});
test('checkpoint policy controls WAL across reopen and rejects unsafe thresholds',()=>{
 const dir=mkdtempSync(join(tmpdir(),'recern-node-checkpoint-'));try{
  const path=join(dir,'db.rvec');let db=Database.create(path);
  assert.deepEqual(db.checkpointPolicy,{maxWalBytes:67108864,maxCommits:1000});
  db.setCheckpointPolicy({maxCommits:2});db.createCollection('c',{dim:2}).upsert('a',f([1,0]));db.save();
  assert.equal(db.persistenceStats().walCommits,1);
  db=new Database(path);assert.equal(db.persistenceStats().walCommits,1);
  db.setCheckpointPolicy({maxCommits:2});db.collection('c').upsert('b',f([0,1]));db.save();
  assert.deepEqual(Object.fromEntries(["walBytes","walCommits","automaticCheckpoints","lastCheckpointError"].map(k=>[k,db.persistenceStats()[k]])),{walBytes:24,walCommits:0,automaticCheckpoints:1,lastCheckpointError:null});
  assert.equal(new Database(path).collection('c').stats().live,2);
  db.setCheckpointPolicy({});assert.deepEqual(db.checkpointPolicy,{maxWalBytes:null,maxCommits:null});
  for(const value of [0,-1,1.5,Infinity,NaN,Number.MAX_SAFE_INTEGER+1]){
   assert.throws(()=>db.setCheckpointPolicy({maxCommits:value}),/positive safe integers/);
   assert.deepEqual(db.checkpointPolicy,{maxWalBytes:null,maxCommits:null});
  }
  assert.throws(()=>Database.openReadOnly(path).setCheckpointPolicy({maxCommits:1}),/read-only/);
 }finally{rmSync(dir,{recursive:true,force:true});}
});
test('paged counters and explicit legacy upgrade retain a readable backup',()=>{
 const fs=require('node:fs');const dir=mkdtempSync(join(tmpdir(),'recern-node-paged-'));
 try {
  const db=Database.create(join(dir,'db.rvec')); assert.equal(db.formatVersion,3);assert.equal(db.upgradeStorage(),null);
  const c=db.createCollection('c',{dim:2});for(let i=0;i<30;i++)c.upsert(String(i),f([i,1]));
  db.checkpoint();c.delete('3');db.save();
  assert.equal(db.persistenceStats().lastEncodedRecords,1);assert.equal(db.persistenceStats().lastWrittenPages,2);assert.equal(db.persistenceStats().lastValidatedBytes,4124);
  db.save();assert.equal(db.persistenceStats().lastEncodedRecords,0);
  const path=join(dir,'legacy.rvec');fs.copyFileSync(join(__dirname,'../../recern-vector/tests/fixtures/v2-0.2.0.rvec'),path);
  const before=fs.readFileSync(path);const legacy=new Database(path);assert.equal(legacy.formatVersion,2);
  const backup=legacy.upgradeStorage();assert.deepEqual(fs.readFileSync(backup),before);assert.equal(legacy.formatVersion,3);
  assert.equal(Database.openReadOnly(backup).formatVersion,2);assert.equal(new Database(path).collection('int8').get('alpha').id,'alpha');
  assert.throws(()=>Database.openReadOnly(path).upgradeStorage(),/read-only/);
 }finally{rmSync(dir,{recursive:true,force:true});}
});
