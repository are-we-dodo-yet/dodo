#!/usr/bin/env -S deno run --allow-read --allow-write
// Copyright 2018-2026 the Deno authors. MIT license.

// deno-lint-ignore-file camelcase

// Converts the bench.json written by tests/bench/main.rs into
// github-action-benchmark's `customSmallerIsBetter` format, one file per
// dashboard section.
//
// Usage: tools/build_benchmark_jsons.ts <bench.json> <out dir>

interface BenchmarkResult {
  name: string;
  unit: string;
  value: number;
  range?: string;
  extra?: string;
}

interface BenchJson {
  // hyperfine results, in seconds.
  exec_time: Record<string, {
    mean: number;
    stddev: number;
    min: number;
    max: number;
    user: number;
    system: number;
  }>;
  // Bytes.
  binary_size: Record<string, number>;
  cargo_deps: number;
  // Milliseconds.
  lsp_exec_time: Record<string, number>;
  // Bytes.
  max_memory: Record<string, number>;
  syscall_count: Record<string, number>;
  thread_count: Record<string, number>;
}

// One section per chart on deno.com/benchmarks.
export const BENCHMARK_SECTIONS: {
  name: string;
  results: (json: BenchJson) => BenchmarkResult[];
}[] = [
  {
    name: "Execution Time",
    results: (json) =>
      Object.keys(json.exec_time).sort().map((name) => {
        const r = json.exec_time[name];
        return {
          name,
          unit: "ms",
          value: toMs(r.mean),
          range: `± ${toMs(r.stddev)}`,
          extra: `min ${toMs(r.min)} ms, max ${toMs(r.max)} ms, ` +
            `user ${toMs(r.user)} ms, system ${toMs(r.system)} ms`,
        };
      }),
  },
  {
    name: "Thread Count",
    results: (json) => metrics(json.thread_count, "threads"),
  },
  {
    name: "Syscall Count",
    results: (json) => metrics(json.syscall_count, "syscalls"),
  },
  {
    name: "Max Memory Usage",
    results: (json) => metrics(json.max_memory, "MiB", toMiB),
  },
  {
    name: "File Sizes",
    results: (json) =>
      metrics(json.binary_size, "MiB", toMiB)
        .filter((r) => !isSnapshot(r.name)),
  },
  {
    name: "Snapshot File Sizes",
    results: (json) =>
      metrics(json.binary_size, "MiB", toMiB)
        .filter((r) => isSnapshot(r.name)),
  },
  {
    name: "Cargo Dependencies",
    results: (json) => [
      { name: "cargo_deps", unit: "dependencies", value: json.cargo_deps },
    ],
  },
  {
    name: "LSP Performance",
    results: (json) => metrics(json.lsp_exec_time, "ms"),
  },
];

export function sectionFile(name: string): string {
  return `${name.toLowerCase().replaceAll(" ", "_")}.json`;
}

function metrics(
  record: Record<string, number>,
  unit: string,
  scale = (n: number) => n,
): BenchmarkResult[] {
  return Object.keys(record).sort().map((name) => ({
    name,
    unit,
    value: scale(record[name]),
  }));
}

function isSnapshot(name: string): boolean {
  return name.endsWith("_SNAPSHOT.bin");
}

function round3(n: number): number {
  return Math.round(n * 1000) / 1000;
}

function toMs(seconds: number): number {
  return round3(seconds * 1000);
}

function toMiB(bytes: number): number {
  return round3(bytes / 2 ** 20);
}

if (import.meta.main) {
  const [benchJsonPath, outDir] = Deno.args;
  if (!benchJsonPath || !outDir) {
    throw new Error("Usage: build_benchmark_jsons.ts <bench.json> <out dir>");
  }
  const json: BenchJson = JSON.parse(Deno.readTextFileSync(benchJsonPath));
  Deno.mkdirSync(outDir, { recursive: true });
  for (const { name, results } of BENCHMARK_SECTIONS) {
    const data = results(json);
    if (data.length === 0) {
      throw new Error(`${benchJsonPath} has no "${name}" results`);
    }
    Deno.writeTextFileSync(
      `${outDir}/${sectionFile(name)}`,
      JSON.stringify(data, null, 2) + "\n",
    );
  }
}
