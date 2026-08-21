/**
 * Glue for the hand-written WebAssembly ABI in
 * `extensions/shared/wasm/src/lib.rs`.
 *
 * Buffers cross the boundary as (pointer, length) pairs into the module's
 * linear memory. Functions returning a buffer return a packed u64 — pointer in
 * the high 32 bits, length in the low 32 — and the caller owns the result.
 * Every read here frees what it read, so a long-running background page does
 * not leak linear memory one decision at a time.
 */

import type {
  CosmeticResponse,
  EngineConfig,
  FilterResult,
  RequestContext,
} from './types.js';

interface Exports {
  memory: WebAssembly.Memory;
  rb_alloc(len: number): number;
  rb_dealloc(ptr: number, len: number): void;
  rb_version(): bigint;
  rb_last_error(): bigint;
  rb_engine_new(
    dbPtr: number,
    dbLen: number,
    cfgPtr: number,
    cfgLen: number,
    userPtr: number,
    userLen: number,
  ): number;
  rb_engine_free(handle: number): void;
  rb_set_config(handle: number, ptr: number, len: number): number;
  rb_evaluate(handle: number, ptr: number, len: number): bigint;
  rb_cosmetic(handle: number, ptr: number, len: number): bigint;
  rb_cosmetic_css(handle: number, ptr: number, len: number): bigint;
  rb_stats(handle: number): bigint;
  rb_compile_dnr(ptr: number, len: number, firstId: number): bigint;
}

export interface EngineStats {
  rules: number;
  dropped: number;
  load: Record<string, number>;
  sources: Array<{
    id: string;
    title?: string | null;
    version?: string | null;
    license?: string | null;
    rule_count: number;
  }>;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export class RatBlockerEngine {
  private constructor(
    private readonly exports: Exports,
    private handle: number,
  ) {}

  /**
   * Instantiate the module and build an engine from a compiled rule database.
   */
  static async create(
    wasmUrl: string,
    database: Uint8Array,
    config: EngineConfig,
    userRules: string,
  ): Promise<RatBlockerEngine> {
    const bytes = await (await fetch(wasmUrl)).arrayBuffer();
    // The module imports nothing, so an empty import object is complete.
    const { instance } = await WebAssembly.instantiate(bytes, {});
    const exports = instance.exports as unknown as Exports;

    const engine = new RatBlockerEngine(exports, 0);
    const db = engine.write(database);
    const cfg = engine.writeString(JSON.stringify(config));
    const user = engine.writeString(userRules);
    try {
      const handle = exports.rb_engine_new(
        db.ptr,
        db.len,
        cfg.ptr,
        cfg.len,
        user.ptr,
        user.len,
      );
      if (handle === 0) {
        throw new Error(`RatBlocker core refused the rule database: ${engine.lastError()}`);
      }
      engine.handle = handle;
      return engine;
    } finally {
      engine.free(db);
      engine.free(cfg);
      engine.free(user);
    }
  }

  get version(): string {
    return this.readPacked(this.exports.rb_version());
  }

  stats(): EngineStats {
    return JSON.parse(this.readPacked(this.exports.rb_stats(this.handle))) as EngineStats;
  }

  setConfig(config: EngineConfig): void {
    const buf = this.writeString(JSON.stringify(config));
    try {
      if (this.exports.rb_set_config(this.handle, buf.ptr, buf.len) !== 0) {
        throw new Error(`RatBlocker core rejected the configuration: ${this.lastError()}`);
      }
    } finally {
      this.free(buf);
    }
  }

  evaluate(context: RequestContext): FilterResult {
    const buf = this.writeString(JSON.stringify(context));
    try {
      return JSON.parse(
        this.readPacked(this.exports.rb_evaluate(this.handle, buf.ptr, buf.len)),
      ) as FilterResult;
    } finally {
      this.free(buf);
    }
  }

  cosmetic(pageUrl: string): CosmeticResponse {
    const buf = this.writeString(pageUrl);
    try {
      return JSON.parse(
        this.readPacked(this.exports.rb_cosmetic(this.handle, buf.ptr, buf.len)),
      ) as CosmeticResponse;
    } finally {
      this.free(buf);
    }
  }

  cosmeticCss(pageUrl: string): string {
    const buf = this.writeString(pageUrl);
    try {
      return this.readPacked(this.exports.rb_cosmetic_css(this.handle, buf.ptr, buf.len));
    } finally {
      this.free(buf);
    }
  }

  /**
   * Compile Adblock-syntax text into Chromium declarativeNetRequest rules,
   * using the same parser and converter as the bundled lists.
   */
  compileDnr(
    text: string,
    firstId: number,
  ): { rules: unknown[]; problems: string[] } {
    const buf = this.writeString(text);
    try {
      return JSON.parse(
        this.readPacked(this.exports.rb_compile_dnr(buf.ptr, buf.len, firstId)),
      ) as { rules: unknown[]; problems: string[] };
    } finally {
      this.free(buf);
    }
  }

  dispose(): void {
    if (this.handle !== 0) {
      this.exports.rb_engine_free(this.handle);
      this.handle = 0;
    }
  }

  // -- ABI plumbing --------------------------------------------------------

  private lastError(): string {
    return this.readPacked(this.exports.rb_last_error());
  }

  private write(data: Uint8Array): { ptr: number; len: number } {
    const ptr = this.exports.rb_alloc(data.length);
    new Uint8Array(this.exports.memory.buffer, ptr, data.length).set(data);
    return { ptr, len: data.length };
  }

  private writeString(s: string): { ptr: number; len: number } {
    return this.write(encoder.encode(s));
  }

  private free(buf: { ptr: number; len: number }): void {
    this.exports.rb_dealloc(buf.ptr, buf.len);
  }

  /** Read a packed pointer/length result and release the buffer behind it. */
  private readPacked(packed: bigint): string {
    const ptr = Number(packed >> 32n);
    const len = Number(packed & 0xffffffffn);
    if (ptr === 0 || len === 0) {
      if (ptr !== 0) this.exports.rb_dealloc(ptr, len);
      return '';
    }
    // Copy before freeing: the view aliases linear memory, which rb_dealloc
    // may hand straight back out to the next allocation.
    const view = new Uint8Array(this.exports.memory.buffer, ptr, len);
    const text = decoder.decode(view.slice());
    this.exports.rb_dealloc(ptr, len);
    return text;
  }
}
