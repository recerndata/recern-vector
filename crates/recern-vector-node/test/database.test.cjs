const {test}=require('node:test');const assert=require('node:assert/strict');
const {mkdtempSync,rmSync}=require('node:fs');const {tmpdir}=require('node:os');const {join}=require('node:path');
const {Database,version,formatVersion}=require('..');
const f=(a)=>new Float32Array(a);
test('native database, quantization, filters, WAL, async buffer ownership and checkpoint',async()=>{
 const dir=mkdtempSync(join(tmpdir(),'recern-node-'));try{
  assert.equal(version(),'0.2.0');assert.equal(formatVersion(),2);
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
