'use strict';
const {PortableSession} = require('../recern_vector.node');

class PortableError extends Error {
  constructor(value) {
    super(`Recern portable ${value.code} (write outcome: ${value.write_outcome})`);
    this.name = value.code;
    this.code = value.code;
    this.requestId = value.request_id;
    this.writeOutcome = value.write_outcome;
  }
}
function invalid() {
  return new PortableError({code:'InvalidArgument', request_id:null, write_outcome:'not_applicable'});
}
function capabilities(v) {
  return Object.freeze({backend:v.backend, profile:v.profile, atomicBatches:v.atomic_batches,
    consistentExport:v.consistent_export, quantization:Object.freeze(v.quantization),
    maxBatch:v.max_batch, exactSearch:v.exact_search, snapshotPages:v.snapshot_pages});
}
function record(v) { return v === null ? null : {id:v.id, vector:v.vector, metadataJson:v.metadata_json}; }
const internal = Symbol('portable client');

class Client {
  #session; #view; #closed = false;
  constructor(token, session, view, info) {
    if (token !== internal) throw invalid();
    this.#session = session; this.#view = view;
    Object.defineProperties(this, {identity:{value:info.identity, enumerable:true},
      capabilities:{value:capabilities(info.capabilities), enumerable:true}});
  }
  static async #connect(options) {
    const session = new PortableSession(JSON.stringify(options));
    const value = JSON.parse(await session.execute(JSON.stringify({operation:'info',params:{}})));
    if (value.error) throw new PortableError(value.error);
    return new Client(internal, session, 'root', value.ok);
  }
  static embedded(path, {readOnly = false} = {}) { return Client.#connect({backend:'embedded',path,readOnly}); }
  static remote(url, {apiKey, timeoutMs = 10000, caFile} = {}) { return Client.#connect({backend:'remote',url,apiKey,timeoutMs,caFile}); }
  static qdrant(url, {apiKey, timeoutMs = 10000, caFile, namespace = 'recern'} = {}) { return Client.#connect({backend:'qdrant',url,apiKey,timeoutMs,caFile,namespace}); }
  async _call(operation, params = {}) {
    if (this.#closed) throw new PortableError({code:'Unavailable',request_id:null,write_outcome:'not_applicable'});
    const value = JSON.parse(await this.#session.execute(JSON.stringify({view:this.#view,operation,params})));
    if (value.error) throw new PortableError(value.error);
    return value.ok;
  }
  listCollections() { return this._call('list'); }
  async createCollection(name, {dim, metric = 'cosine', quantization = 'f32', creationToken} = {}) {
    if (creationToken !== undefined && typeof creationToken !== 'string') throw invalid();
    return new Collection(internal, this, await this._call('create', {spec:{name,dim,metric,quantization},creationToken}));
  }
  async collection(name) { return new Collection(internal, this, await this._call('collection',{name})); }
  dropCollection(name) { return this._call('drop',{name}); }
  async snapshot() {
    const info = await this._call('snapshot');
    return new Client(internal,this.#session,info.view,info);
  }
  async close() {
    if (this.#closed) return;
    try { await this._call('close'); } finally { this.#closed = true; }
  }
  [Symbol.asyncDispose]() { return this.close(); }
}

class Collection {
  #client; #descriptor;
  constructor(token, client, descriptor) {
    if (token !== internal) throw invalid();
    this.#client = client; this.#descriptor = Object.freeze(descriptor);
    Object.defineProperties(this, {name:{value:descriptor.name,enumerable:true},
      identity:{value:descriptor.identity,enumerable:true},
      spec:{value:Object.freeze({name:descriptor.name,dim:descriptor.dim,metric:descriptor.metric,quantization:descriptor.quantization}),enumerable:true}});
  }
  #call(operation, params = {}) { return this.#client._call(operation,{...params,collection:this.#descriptor}); }
  async upsert(id, vector, metadataJson = 'null') { await this.upsertMany([{id,vector,metadataJson}]); }
  async upsertMany(records) {
    if (!Array.isArray(records)) throw invalid();
    return this.#call('upsert',{records:records.map(r => {
      if (!r || !(Array.isArray(r.vector) || r.vector instanceof Float32Array)) throw invalid();
      return {id:r.id,vector:Array.from(r.vector),metadata_json:r.metadataJson === undefined ? 'null' : r.metadataJson};
    })});
  }
  async get(id) { return record(await this.#call('get',{id})); }
  delete(id) { return this.#call('delete',{id}); }
  async count() { return BigInt(await this.#call('count')); }
  async page({limit = 100, cursor = null} = {}) {
    if (cursor !== null && typeof cursor !== 'string') throw invalid();
    const value = await this.#call('page',{limit,cursor});
    return {records:value.records.map(record),nextCursor:value.next_cursor};
  }
  async *iterRecords({batchSize = 100} = {}) {
    let cursor = null;
    do {
      const page = await this.page({limit:batchSize,cursor});
      yield* page.records; cursor = page.nextCursor;
    } while (cursor !== null);
  }
  async search(query, {k = 10, filterJson = null, exact = false} = {}) {
    if (!(Array.isArray(query) || query instanceof Float32Array) || (filterJson !== null && typeof filterJson !== 'string')) throw invalid();
    const hits = await this.#call('search',{query:Array.from(query),k,filterJson,exact});
    return hits.map(h => ({id:h.id,distance:h.distance,metadataJson:h.metadata_json}));
  }
}
module.exports = {Client, Collection, PortableError, API_VERSION:1, PROFILE:'recern-vector/1'};
