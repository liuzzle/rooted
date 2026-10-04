import { describe, expect, it } from "vitest";
import { bounds, createSim, pin, position, settle, step, unpin } from "./forceLayout";

const triangle = () =>
  createSim(
    [1, 2, 3, 4],
    [
      { a: 1, b: 2, passages: 4 },
      { a: 2, b: 3, passages: 1 },
    ],
  );

const dist = (p: { x: number; y: number }, q: { x: number; y: number }) =>
  Math.hypot(p.x - q.x, p.y - q.y);

describe("force layout", () => {
  it("lays the same graph out the same way every time", () => {
    const a = triangle();
    const b = triangle();
    settle(a);
    settle(b);
    expect(Array.from(a.xs)).toEqual(Array.from(b.xs));
    expect(Array.from(a.ys)).toEqual(Array.from(b.ys));
  });

  it("comes to rest", () => {
    const sim = triangle();
    settle(sim);
    expect(step(sim, 0.5)).toBeLessThanOrEqual(0.5);
  });

  it("draws topics written together closer than topics that never were", () => {
    const sim = triangle();
    settle(sim);
    const p1 = position(sim, 1)!;
    expect(dist(p1, position(sim, 2)!)).toBeLessThan(dist(p1, position(sim, 4)!));
  });

  it("holds a dragged topic where the pointer is, and its neighbour follows", () => {
    const sim = triangle();
    settle(sim);
    const before = position(sim, 2)!;
    pin(sim, 1, 2000, 0);
    for (let i = 0; i < 200; i++) step(sim, sim.k * 0.25);
    expect(position(sim, 1)).toEqual({ x: 2000, y: 0 });
    expect(position(sim, 2)!.x).toBeGreaterThan(before.x + 100);

    unpin(sim);
    step(sim, 10);
    expect(position(sim, 1)!.x).not.toBe(2000);
  });

  it("keeps a large sparse graph compact, without topics flung to the edges", () => {
    const ids = Array.from({ length: 150 }, (_, i) => i + 1);
    const links = ids.flatMap((i) =>
      i % 5 === 0 ? [] : [{ a: i, b: i + 1, passages: 1 }].filter((l) => l.b <= 150),
    );
    const sim = createSim(ids, links);
    settle(sim);
    const r = Array.from(sim.xs, (x, i) => Math.hypot(x, sim.ys[i])).sort((a, b) => a - b);
    const median = r[r.length >> 1];
    expect(r[r.length - 1] / median).toBeLessThan(2.5);
    // Within the area the layout is sized for (1000 × 700), give or take.
    expect(r[r.length - 1]).toBeLessThan(1000);
  });

  it("ignores a link to a topic that isn't drawn", () => {
    const sim = createSim([1, 2], [{ a: 1, b: 99, passages: 1 }]);
    expect(sim.edges).toEqual([]);
  });

  it("fits every topic inside the view", () => {
    const sim = triangle();
    settle(sim);
    const box = bounds(sim, 10);
    for (const id of [1, 2, 3, 4]) {
      const p = position(sim, id)!;
      expect(p.x).toBeGreaterThan(box.x);
      expect(p.x).toBeLessThan(box.x + box.w);
      expect(p.y).toBeGreaterThan(box.y);
      expect(p.y).toBeLessThan(box.y + box.h);
    }
  });
});
