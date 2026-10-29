// Run against a source-built or npm-packed installation. Read credentials from
// stdin, never argv. Metadata stays text even when the envelope uses V8 JSON.
const fs=require('node:fs');
const assert=require('node:assert/strict');
const {Client}=require(process.env.RECERN_NODE_PORTABLE || '../crates/recern-vector-node/portable');
(async ()=> {
  const config=JSON.parse(fs.readFileSync(0,'utf8'));
  const client=config.backend === 'embedded' ? await Client.embedded(config.path)
    : config.backend === 'remote' ? await Client.remote(config.url,config)
    : await Client.qdrant(config.url,config);
  try {
    const col=await client.collection('interop');
    const original=await col.get('python/東京\n\0');
    assert.match(original.metadataJson,/18446744073709551615/);
    assert.match(original.metadataJson,/"float":\s*\[1\.0\]/);
    await col.upsert('node',[1,0],original.metadataJson);
    assert.equal((await col.search([1,0],{exact:true,filterJson:'{"float":[1.0]}'})).length,3);
    assert.equal((await col.search([1,0],{exact:true,filterJson:'{"float":[1]}'})).length,0);
    console.log(JSON.stringify({verified:true,count:(await col.count()).toString()}));
  } finally { await client.close(); }
})().catch(e=> { console.error(e); process.exitCode=1; });
