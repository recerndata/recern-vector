const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const {spawn} = require('node:child_process');
const {randomUUID} = require('node:crypto');
const {Client, PortableError} = require('../portable');
const source = fs.readFileSync(path.resolve(__dirname,'../../recern-vector/tests/fixtures/portable-v1.json'),'utf8');
const fixture = JSON.parse(source);
// rawJSON retains the fixture's nested 1 versus 1.0 instead of silently
// weakening the Node contract suite to V8's single numeric type.
const rawFixture = JSON.parse(source, (_key,v,context) => typeof v === 'number' ? JSON.rawJSON(context.source) : v);
const key = 'recern-node-tests-local-only-000000000000';

async function server(t, dir) {
  const keyFile = path.join(dir,'key'); fs.writeFileSync(keyFile,key);
  const child = spawn(process.env.RECERN_TEST_SERVER_BIN,['--database',path.join(dir,'server.rvec'),'--api-key-file',keyFile,'--listen','127.0.0.1:0'],{stdio:['ignore','ignore','pipe']});
  let log = '';
  t.after(async () => { if (child.exitCode === null) { child.kill(); await new Promise(resolve => child.once('exit',resolve)); } });
  return new Promise((resolve,reject) => {
    const timer = setTimeout(()=>reject(Error('server startup timeout')),10000);
    child.stderr.on('data',data=> { log += data; const match = log.match(/listening on (127\.0\.0\.1:\d+)/); if (match) {clearTimeout(timer); resolve('http://'+match[1]);} });
    child.once('error',e=>{clearTimeout(timer);reject(e);});
    child.once('exit',()=>{clearTimeout(timer);reject(Error('server exited: '+log));});
  });
}
for (const backend of ['embedded','remote','qdrant']) {
  test(`portable ${backend}: frozen profile and generation handles`,{skip:backend === 'remote' && !process.env.RECERN_TEST_SERVER_BIN || backend === 'qdrant' && !process.env.RECERN_TEST_QDRANT_URL},async t=> {
    const dir=fs.mkdtempSync(path.join(os.tmpdir(),'recern-node-portable-'));
    const client=backend === 'embedded' ? await Client.embedded(path.join(dir,'native.rvec'))
      : backend === 'remote' ? await Client.remote(await server(t,dir),{apiKey:key})
      : await Client.qdrant(process.env.RECERN_TEST_QDRANT_URL,{apiKey:process.env.RECERN_TEST_QDRANT_API_KEY,namespace:'node_'+randomUUID().replaceAll('-','').slice(0,16)});
    t.after(()=>client.close());
    // after hooks run in registration order. In particular Windows must
    // release the server ownership lock before deleting its directory.
    t.after(()=>fs.rmSync(dir,{recursive:true,force:true}));
    for (const metric of ['cosine','l2','dot']) {
      const c=await client.createCollection('collection/世界/'+metric,{dim:2,metric});
      assert.equal(await c.upsertMany(fixture.records.map((r,i)=>({...r,metadataJson:JSON.stringify(rawFixture.records[i].metadata)}))),fixture.records.length);
      assert.equal(await c.count(),BigInt(fixture.records.length));
      for (let i=0;i<fixture.filters.length;i++) {
        const item=fixture.filters[i];
        const hits=await c.search([.6,.8],{k:20,filterJson:JSON.stringify(rawFixture.filters[i].filter),exact:true});
        assert.deepEqual(hits.map(h=>h.id).sort(),[...item.ids].sort(),JSON.stringify(item));
        for (const h of hits) {
          const r=fixture.records.find(r=>r.id === h.id);
          assert.deepEqual(JSON.parse(h.metadataJson),r.metadata);
          const v=r.vector;
          const distance=metric === 'cosine' ? Math.max(0,1-(v[0]*.6+v[1]*.8)/Math.hypot(...v)) : metric === 'l2' ? (v[0]-.6)**2+(v[1]-.8)**2 : -(v[0]*.6+v[1]*.8);
          assert.ok(Math.abs(h.distance-distance)<2e-6);
        }
      }
      for (const filter of fixture.invalid_filters) {
        await assert.rejects(c.search([1,0],{filterJson:JSON.stringify(filter)}),{code:'InvalidArgument'});
      }
      const records=[]; for await (const r of c.iterRecords({batchSize:2})) records.push(r);
      assert.deepEqual(records.map(r=>r.id).sort(),fixture.records.map(r=>r.id).sort());
      await assert.rejects(c.upsertMany([{id:'valid',vector:[1,0]},{id:'bad',vector:[1]}]),{code:'InvalidArgument'});
      await assert.rejects(c.upsertMany([{id:'same',vector:[1,0]},{id:'same',vector:[1,0]}]),{code:'InvalidArgument'});
      await assert.rejects(c.upsert('bad',[1,0],'{"a":1,"a":2}'),{code:'InvalidArgument'});
      assert.equal(await c.count(),BigInt(fixture.records.length));
      await c.upsert('numeric',[1,0],'{"u64":18446744073709551615,"float":[1.0],"int":[1]}');
      assert.match((await c.get('numeric')).metadataJson,/18446744073709551615/);
      assert.equal((await c.search([1,0],{exact:true,filterJson:'{"float":[1.0]}'}))[0].id,'numeric');
      assert.deepEqual(await c.search([1,0],{exact:true,filterJson:'{"float":[1]}'}),[]);
      assert.equal(await c.delete('numeric'),true); assert.equal(await c.delete('numeric'),false); assert.equal(await c.get('numeric'),null);
      if (client.capabilities.consistentExport) {
        const snapshot=await client.snapshot();
        try {
          await c.upsert('new',[1,1]);
          const frozen=await snapshot.collection(c.name);
          assert.equal(await frozen.count(),BigInt(fixture.records.length));
          await assert.rejects(frozen.upsertMany([]),{code:'PermissionDenied'});
        } finally { await snapshot.close(); }
      } else { await assert.rejects(client.snapshot(),{code:'Unsupported'}); }
      await client.dropCollection(c.name);
      const replacement=await client.createCollection(c.name,{dim:2,metric});
      assert.notEqual(replacement.identity,c.identity);
      await assert.rejects(c.upsertMany([]),{code:'Conflict'});
      await client.dropCollection(c.name);
    }
    assert.deepEqual(await client.listCollections(),[]);
  });
}
test('portable transport rejects unsafe configurations without network',async ()=> {
  for (const url of ['http://example.com','https://user:pass@example.com','http://localhost\n','https://example.com?q=x']) {
    await assert.rejects(Client.remote(url,{apiKey:key}),{code:'InvalidArgument'});
  }
  assert.throws(()=>new Client(),PortableError);
});
