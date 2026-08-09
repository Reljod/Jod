interface Props {
  /** Event timestamps, ascending. */
  times: number[];
  now: number;
  live: boolean;
  windowMs?: number;
  buckets?: number;
}

/**
 * Event density over the last half-minute, as an inline SVG.
 *
 * A flat line on a "running" agent is the signal worth catching — it means the
 * run is alive but producing nothing, which the status field alone will never
 * tell you.
 */
export function Sparkline({ times, now, live, windowMs = 30000, buckets = 24 }: Props) {
  const start = now - windowMs;
  const bins = new Array<number>(buckets).fill(0);
  for (let i = times.length - 1; i >= 0; i--) {
    const t = times[i];
    if (t < start) break;
    const idx = Math.min(buckets - 1, Math.floor(((t - start) / windowMs) * buckets));
    bins[idx] += 1;
  }
  const peak = Math.max(1, ...bins);
  const w = 62;
  const h = 12;
  const step = w / buckets;

  return (
    <svg className="spark" width={w} height={h} viewBox={`0 0 ${w} ${h}`} aria-hidden="true">
      {bins.map((v, i) => {
        const bh = Math.max(v > 0 ? 1.5 : 0.7, (v / peak) * h);
        return (
          <rect
            key={i}
            x={i * step}
            y={h - bh}
            width={Math.max(1, step - 1)}
            height={bh}
            className={v > 0 ? (live ? "on" : "past") : "off"}
          />
        );
      })}
    </svg>
  );
}
