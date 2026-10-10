export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export interface CollectionOptions { dim: number; metric?: 'cosine' | 'l2' | 'dot'; quantization?: 'f32' | 'int8'; m?: number; efConstruction?: number; efSearch?: number }
export interface SearchOptions { ef?: number; exact?: boolean; filter?: { [key: string]: Json } }
export interface Hit { id: string; distance: number; metadata: Json }
export interface Record { id: string; vector: number[]; metadata: Json }
export interface SearchReport { hits: Hit[]; strategy: 'hnsw' | 'exact' | 'filtered_exact'; ef: number | null; visited: number; distanceComputations: number; filterSelectivity: number | null; elapsedMs: number }
export interface Stats { name: string; dim: number; metric: string; quantization: string; live: number; deleted: number; vectorBytes: number; graphBytes: number; metadataBytes: number; unreachable: number; nodesPerLayer: number[] }
export class Database {
  constructor(path: string);
  static create(path: string): Database;
  static openOrCreate(path: string): Database;
  static openReadOnly(path: string): Database;
  readonly readOnly: boolean;
  createCollection(name: string, options: CollectionOptions): Collection;
  collection(name: string): Collection;
  collectionNames(): string[];
  dropCollection(name: string): void;
  save(): void;
  checkpoint(): void;
}
export class Collection {
  private constructor();
  upsert(id: string, vector: Float32Array, metadata?: Json): void;
  upsertMany(records: { id: string; vector: Float32Array; metadata?: Json }[]): number;
  get(id: string): Record | null;
  delete(id: string): boolean;
  search(query: Float32Array, k: number, options?: SearchOptions): Hit[];
  searchAsync(query: Float32Array, k: number, options?: SearchOptions): Promise<Hit[]>;
  explain(query: Float32Array, k: number, options?: SearchOptions): SearchReport;
  compact(): number;
  stats(): Stats;
}
export function version(): string;
export function formatVersion(): number;
