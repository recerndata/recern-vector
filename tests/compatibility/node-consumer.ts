// Compile this external consumer against npm's packed exports with --strict.
import {Database, Collection, Hit, Json, SearchReport, Stats, version, formatVersion} from '@recern/vector';
import {Client, PortableError, Record, Page, PROFILE, API_VERSION} from '@recern/vector/portable';

const db: Database = Database.openOrCreate('native.rvec');
const collection: Collection = db.createCollection('docs', {dim: 2, metric: 'l2', efSearch: 64});
const metadata: Json = {label: 'test', items: [null, true, 1]};
collection.upsert('a', new Float32Array([1, 0]), metadata);
const report: SearchReport = collection.explain(new Float32Array([1, 0]), 1, {exact: true});
const stats: Stats = collection.stats();
const hits: Promise<Hit[]> = collection.searchAsync(new Float32Array([1, 0]), 1);
db.setCheckpointPolicy({maxCommits: 2});
const error: string | null = db.persistenceStats().lastCheckpointError;
const upgrade: string | null = db.upgradeStorage();
db.save(); db.checkpoint();
void [report, stats, hits, error, upgrade, version(), formatVersion()];

async function portable(): Promise<void> {
  await using client = await Client.embedded('portable.rvec');
  const c = await client.createCollection('docs', {dim: 2});
  await c.upsert('a', [1, 0], '{"integer":18446744073709551615}');
  const count: bigint = await c.count();
  const record: Record | null = await c.get('a');
  const page: Page = await c.page({limit: 100});
  for await (const r of c.iterRecords({batchSize: 100})) {
    const json: string = r.metadataJson;
    void json;
  }
  const failure = new PortableError({code: 'Unavailable', request_id: null, write_outcome: 'unknown'});
  const outcome: 'unknown' | 'not_applicable' | 'not_committed' = failure.writeOutcome;
  void [count, record, page, outcome, PROFILE, API_VERSION];
}
void portable;
