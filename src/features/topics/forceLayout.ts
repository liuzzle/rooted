/**
 * Force layout for the topic graph — Fruchterman–Reingold, steppable.
 *
 * The same forces serve two jobs: settling the graph before it is first drawn
 * (so it never opens in a jitter), and running live while a topic is dragged,
 * so its neighbours follow. A pinned node is held where the pointer is and
 * pushes and pulls the others like any node.
 *
 * Deterministic: positions start on a spiral, not at random, so the same graph
 * opens the same way each visit and a topic stays where you last saw it.
 */

export interface Sim {
  ids: number[];
  index: Map<number, number>;
  xs: Float64Array;
  ys: Float64Array;
  /** [i, j, passages] — node indices, and how many passages join them. */
  edges: [number, number, number][];
  /** Ideal distance between nodes. */
  k: number;
  /** Index of a node held in place, or -1. */
  pinned: number;
}

const AREA = 1000 * 700;
/**
 * Pull toward the centre, per unit of distance. Every pair of topics repels
 * (k²/d), so with n topics the graph only stops growing where gravity matches
 * about n·k²/R — at R ≈ k·√(n/G). G = 1 keeps that near k·√n, the area the
 * layout is sized for. At 0.02 the graph spread to forty times that and was
 * drawn as a thin streak with a few topics flung to the edges.
 */
const GRAVITY = 1;

export function createSim(
  ids: number[],
  links: { a: number; b: number; passages: number }[],
): Sim {
  const n = ids.length;
  const index = new Map(ids.map((id, i) => [id, i]));
  const k = Math.sqrt(AREA / Math.max(n, 1));
  const xs = new Float64Array(n);
  const ys = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    // Most-reached topics come first, so they start (and tend to stay) central.
    const angle = i * 2.399963; // golden angle
    const r = k * 0.6 * Math.sqrt(i);
    xs[i] = r * Math.cos(angle);
    ys[i] = r * Math.sin(angle);
  }
  const edges = links
    .map((e) => [index.get(e.a), index.get(e.b), e.passages] as const)
    .filter((e): e is [number, number, number] => e[0] != null && e[1] != null)
    .map((e) => [e[0], e[1], e[2]] as [number, number, number]);
  return { ids, index, xs, ys, edges, k, pinned: -1 };
}

/**
 * Move every free node by at most `temperature`. Returns the largest move,
 * so a caller can tell when the graph has come to rest.
 */
export function step(sim: Sim, temperature: number): number {
  const { xs, ys, edges, k, pinned } = sim;
  const n = xs.length;
  const dx = new Float64Array(n);
  const dy = new Float64Array(n);

  // Every pair repels: k²/d.
  for (let i = 0; i < n; i++) {
    for (let j = i + 1; j < n; j++) {
      const ex = xs[i] - xs[j];
      const ey = ys[i] - ys[j];
      const d2 = ex * ex + ey * ey || 0.01;
      const f = (k * k) / d2;
      dx[i] += ex * f;
      dy[i] += ey * f;
      dx[j] -= ex * f;
      dy[j] -= ey * f;
    }
  }
  // Topics written together attract: d²/k, harder the more passages join them.
  for (const [i, j, w] of edges) {
    const ex = xs[i] - xs[j];
    const ey = ys[i] - ys[j];
    const d = Math.sqrt(ex * ex + ey * ey) || 0.01;
    const pull = (d / k) * (1 + Math.log2(Math.max(w, 1)));
    dx[i] -= ex * pull;
    dy[i] -= ey * pull;
    dx[j] += ex * pull;
    dy[j] += ey * pull;
  }

  let moved = 0;
  for (let i = 0; i < n; i++) {
    if (i === pinned) continue;
    // A little gravity, so topics with no lines stay on the page.
    dx[i] -= xs[i] * GRAVITY;
    dy[i] -= ys[i] * GRAVITY;
    const len = Math.sqrt(dx[i] * dx[i] + dy[i] * dy[i]) || 1;
    const move = Math.min(len, temperature);
    xs[i] += (dx[i] / len) * move;
    ys[i] += (dy[i] / len) * move;
    moved = Math.max(moved, move);
  }
  return moved;
}

/** Run to rest, cooling as it goes. Used once, before the first frame. */
export function settle(sim: Sim, iterations = 300): void {
  let temperature = sim.k * 2;
  for (let i = 0; i < iterations; i++) {
    step(sim, temperature);
    temperature *= 0.985;
  }
}

/** Hold a node at a point (graph coordinates). */
export function pin(sim: Sim, id: number, x: number, y: number): void {
  const i = sim.index.get(id);
  if (i == null) return;
  sim.pinned = i;
  sim.xs[i] = x;
  sim.ys[i] = y;
}

export function unpin(sim: Sim): void {
  sim.pinned = -1;
}

export function position(sim: Sim, id: number): { x: number; y: number } | null {
  const i = sim.index.get(id);
  return i == null ? null : { x: sim.xs[i], y: sim.ys[i] };
}

/** Bounding box of every node, padded — the "fit" view. */
export function bounds(sim: Sim, pad = 60) {
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (let i = 0; i < sim.xs.length; i++) {
    minX = Math.min(minX, sim.xs[i]);
    maxX = Math.max(maxX, sim.xs[i]);
    minY = Math.min(minY, sim.ys[i]);
    maxY = Math.max(maxY, sim.ys[i]);
  }
  if (!Number.isFinite(minX)) return { x: -pad, y: -pad, w: 2 * pad, h: 2 * pad };
  return { x: minX - pad, y: minY - pad, w: maxX - minX + 2 * pad, h: maxY - minY + 2 * pad };
}
