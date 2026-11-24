// IPC timings for performance work: per command/event
// count and latency percentiles over the most recent calls. In memory only, nothing is
// sent or written; read in WebView2 DevTools with `riftPerf.ipc()`.
const RECENT = 512;

export interface IpcStat {
  count: number;
  p50: number;
  p95: number;
  p99: number;
  max: number;
}

interface Series {
  count: number;
  max: number;
  recent: number[];
}

/** Nearest-rank percentile of ascending `sorted` (non-empty). */
export function percentile(sorted: number[], p: number): number {
  const rank = Math.ceil((p / 100) * sorted.length);
  return sorted[Math.min(sorted.length, Math.max(1, rank)) - 1];
}

export class IpcMetrics {
  private series = new Map<string, Series>();

  observe(name: string, ms: number) {
    const s = this.series.get(name) ?? { count: 0, max: 0, recent: [] };
    s.count += 1;
    s.max = Math.max(s.max, ms);
    s.recent.push(ms);
    if (s.recent.length > RECENT) s.recent.shift();
    this.series.set(name, s);
  }

  report(): Record<string, IpcStat> {
    const round = (n: number) => Math.round(n * 10) / 10;
    return Object.fromEntries(
      [...this.series].map(([name, s]) => {
        const sorted = [...s.recent].sort((a, b) => a - b);
        return [
          name,
          {
            count: s.count,
            p50: round(percentile(sorted, 50)),
            p95: round(percentile(sorted, 95)),
            p99: round(percentile(sorted, 99)),
            max: round(s.max),
          },
        ];
      }),
    );
  }

  reset() {
    this.series.clear();
  }
}

export const ipcMetrics = new IpcMetrics();

/** Times `call` end to end (request, Rust handler, response) under `name`. */
export async function measured<T>(
  name: string,
  call: () => Promise<T>,
  metrics: IpcMetrics = ipcMetrics,
): Promise<T> {
  const started = performance.now();
  try {
    return await call();
  } finally {
    metrics.observe(name, performance.now() - started);
  }
}

/** Wraps an event listener: count of events and time the frontend spends handling each. */
export function measuredListener<T>(
  name: string,
  listener: (payload: T) => void,
  metrics: IpcMetrics = ipcMetrics,
): (payload: T) => void {
  return (payload) => {
    const started = performance.now();
    try {
      listener(payload);
    } finally {
      metrics.observe(name, performance.now() - started);
    }
  };
}
