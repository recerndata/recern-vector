export type Metric = 'cosine' | 'l2' | 'dot';
export type Quantization = 'f32' | 'int8';
export interface Spec { readonly name: string; readonly dim: number; readonly metric: Metric; readonly quantization: Quantization }
export interface Capabilities {
  readonly backend: 'embedded' | 'recern' | 'qdrant'; readonly profile: 'recern-vector/1';
  readonly atomicBatches: boolean; readonly consistentExport: boolean;
  readonly quantization: readonly Quantization[]; readonly maxBatch: number;
  readonly exactSearch: boolean; readonly snapshotPages: boolean;
}
/** Raw JSON text preserves uint64 and nested integer/float kinds. */
export interface InputRecord { id: string; vector: number[] | Float32Array; metadataJson?: string }
export interface Record { id: string; vector: number[]; metadataJson: string }
export interface Hit { id: string; distance: number; metadataJson: string }
export interface Page { records: Record[]; nextCursor: string | null }
export interface HttpOptions { apiKey?: string; timeoutMs?: number; caFile?: string }
export class PortableError extends Error {
  readonly code: 'InvalidArgument' | 'NotFound' | 'AlreadyExists' | 'Conflict' | 'Unsupported' | 'Unavailable' | 'PermissionDenied' | 'CorruptData';
  readonly requestId: string | null; readonly writeOutcome: 'not_applicable' | 'not_committed' | 'unknown';
}
export class Client {
  private constructor();
  static embedded(path: string, options?: {readOnly?: boolean}): Promise<Client>;
  static remote(url: string, options: HttpOptions & {apiKey: string}): Promise<Client>;
  static qdrant(url: string, options?: HttpOptions & {namespace?: string}): Promise<Client>;
  readonly identity: string; readonly capabilities: Capabilities;
  listCollections(): Promise<string[]>;
  createCollection(name: string, options: {dim: number; metric?: Metric; quantization?: Quantization; creationToken?: string}): Promise<Collection>;
  collection(name: string): Promise<Collection>;
  dropCollection(name: string): Promise<void>;
  snapshot(): Promise<Client>;
  close(): Promise<void>;
}
export class Collection {
  private constructor();
  readonly name: string; readonly identity: string; readonly spec: Spec;
  upsert(id: string, vector: number[] | Float32Array, metadataJson?: string): Promise<void>;
  upsertMany(records: InputRecord[]): Promise<number>;
  get(id: string): Promise<Record | null>;
  delete(id: string): Promise<boolean>;
  count(): Promise<bigint>;
  page(options?: {limit?: number; cursor?: string | null}): Promise<Page>;
  iterRecords(options?: {batchSize?: number}): AsyncIterable<Record>;
  search(query: number[] | Float32Array, options?: {k?: number; filterJson?: string | null; exact?: boolean}): Promise<Hit[]>;
}
export const API_VERSION: 1;
export const PROFILE: 'recern-vector/1';
