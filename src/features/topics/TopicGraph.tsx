import { useEffect, useMemo, useState } from "react";
import {
  ConceptGraph,
  GraphEdge,
  GraphNode,
  SharedPassage,
  conceptGraph,
  sharedPassages,
} from "../../lib/api";
import Marked from "../notes/Marked";

/**
 * The topic graph.
 *
 * A line joins two topics only where one passage of one note mentions both,
 * and clicking it shows those passages. So the graph claims nothing a note
 * doesn't say: "these were written together, here" is the whole meaning of an
 * edge. Similarity is deliberately not drawn — it is a guess, and a line on a
 * graph looks like a fact.
 */
export default function TopicGraph({
  onOpenTopic,
  onOpenNote,
}: {
  onOpenTopic: (conceptId: number) => void;
  onOpenNote: (noteId: number) => void;
}) {
  const [graph, setGraph] = useState<ConceptGraph | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hover, setHover] = useState<number | null>(null);
  const [edge, setEdge] = useState<GraphEdge | null>(null);
  const [passages, setPassages] = useState<SharedPassage[]>([]);

  useEffect(() => {
    conceptGraph(150)
      .then(setGraph)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    if (!edge) return;
    sharedPassages(edge.a, edge.b)
      .then(setPassages)
      .catch((e) => setError(String(e)));
  }, [edge]);

  const layout = useMemo(() => (graph ? layOut(graph) : null), [graph]);

  if (error) return <p className="notes-error">{error}</p>;
  if (!graph || !layout) return null;
  if (graph.nodes.length === 0) {
    return <p className="empty">No topics yet, so nothing to draw.</p>;
  }
  if (graph.edges.length === 0) {
    return (
      <p className="empty">
        No two topics share a passage yet. A line appears when one paragraph
        mentions both.
      </p>
    );
  }

  const byId = new Map(graph.nodes.map((n) => [n.concept_id, n]));
  const neighbours = new Set<number>();
  if (hover != null) {
    for (const e of graph.edges) {
      if (e.a === hover) neighbours.add(e.b);
      if (e.b === hover) neighbours.add(e.a);
    }
  }
  const lit = (id: number) => hover == null || id === hover || neighbours.has(id);

  return (
    <div className="topic-graph">
      <svg
        className="graph-canvas"
        viewBox={layout.viewBox}
        role="img"
        aria-label="Topics, joined where a passage mentions both"
      >
        {graph.edges.map((e) => {
          const a = layout.at.get(e.a)!;
          const b = layout.at.get(e.b)!;
          const on = hover == null || e.a === hover || e.b === hover;
          const chosen = edge && edge.a === e.a && edge.b === e.b;
          return (
            <g
              key={`${e.a}-${e.b}`}
              className={chosen ? "graph-edge chosen" : "graph-edge"}
              opacity={on ? 1 : 0.12}
              onClick={() => setEdge(e)}
            >
              <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} className="graph-edge-hit" />
              <line
                x1={a.x}
                y1={a.y}
                x2={b.x}
                y2={b.y}
                strokeWidth={1 + Math.log2(e.passages)}
              />
              <title>
                {byId.get(e.a)?.label} + {byId.get(e.b)?.label}: {e.passages} passage
                {e.passages === 1 ? "" : "s"}
              </title>
            </g>
          );
        })}
        {graph.nodes.map((n) => {
          const p = layout.at.get(n.concept_id)!;
          return (
            <g
              key={n.concept_id}
              className="graph-node"
              transform={`translate(${p.x},${p.y})`}
              opacity={lit(n.concept_id) ? 1 : 0.2}
              onMouseEnter={() => setHover(n.concept_id)}
              onMouseLeave={() => setHover(null)}
              onClick={() => onOpenTopic(n.concept_id)}
            >
              <circle r={radius(n)} />
              <text y={-radius(n) - 4}>{n.label}</text>
            </g>
          );
        })}
      </svg>

      <aside className="graph-side">
        {!edge && (
          <p className="card-note">
            Click a topic to open it. Click a line to read the passages that
            mention both — a line exists only because one does.
          </p>
        )}
        {edge && (
          <>
            <h3>
              {byId.get(edge.a)?.label} · {byId.get(edge.b)?.label}
            </h3>
            <p className="card-note">
              Written together in {edge.passages} passage
              {edge.passages === 1 ? "" : "s"}.
            </p>
            <ul className="citation-list">
              {passages.map((p, i) => (
                <li key={i} className="citation">
                  <Marked text={p.snippet} marks={p.marks} />
                  <div className="citation-source">
                    <button className="link-btn" onClick={() => onOpenNote(p.note_id)}>
                      {p.note_title ?? `Note ${p.note_id}`}
                    </button>
                    {p.speaker && <span> · {p.speaker}</span>}
                    {p.date && <span> · {p.date}</span>}
                  </div>
                </li>
              ))}
            </ul>
          </>
        )}
      </aside>
    </div>
  );
}

function radius(n: GraphNode) {
  return 4 + 3 * Math.sqrt(n.notes);
}

interface Layout {
  at: Map<number, { x: number; y: number }>;
  viewBox: string;
}

/**
 * Fruchterman–Reingold, run to rest before anything is drawn.
 *
 * Deterministic — positions start on a spiral, not at random — so the same
 * graph is drawn the same way each visit and a topic stays where you last saw
 * it. A few hundred nodes settle in milliseconds; nothing animates.
 */
function layOut(graph: ConceptGraph): Layout {
  const n = graph.nodes.length;
  const index = new Map(graph.nodes.map((node, i) => [node.concept_id, i]));
  const xs = new Float64Array(n);
  const ys = new Float64Array(n);
  const area = 1000 * 700;
  const k = Math.sqrt(area / Math.max(n, 1));
  for (let i = 0; i < n; i++) {
    // Most-reached topics first, so they start (and tend to stay) central.
    const angle = i * 2.399963; // golden angle
    const r = k * 0.6 * Math.sqrt(i);
    xs[i] = r * Math.cos(angle);
    ys[i] = r * Math.sin(angle);
  }
  const edges = graph.edges
    .map((e) => [index.get(e.a), index.get(e.b), e.passages] as const)
    .filter((e): e is [number, number, number] => e[0] != null && e[1] != null);

  const iterations = 300;
  let temperature = k * 2;
  const dx = new Float64Array(n);
  const dy = new Float64Array(n);
  for (let step = 0; step < iterations; step++) {
    dx.fill(0);
    dy.fill(0);
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        let ex = xs[i] - xs[j];
        let ey = ys[i] - ys[j];
        const d2 = ex * ex + ey * ey || 0.01;
        const f = (k * k) / d2;
        ex *= f;
        ey *= f;
        dx[i] += ex;
        dy[i] += ey;
        dx[j] -= ex;
        dy[j] -= ey;
      }
    }
    for (const [i, j, w] of edges) {
      const ex = xs[i] - xs[j];
      const ey = ys[i] - ys[j];
      const d = Math.sqrt(ex * ex + ey * ey) || 0.01;
      // Pull d²/k along the line; topics written together more often, harder.
      const pull = ((d * d) / k) * (1 + Math.log2(w)) / d;
      dx[i] -= ex * pull;
      dy[i] -= ey * pull;
      dx[j] += ex * pull;
      dy[j] += ey * pull;
    }
    for (let i = 0; i < n; i++) {
      // A little gravity, so topics with no lines stay on the page.
      dx[i] -= xs[i] * 0.02;
      dy[i] -= ys[i] * 0.02;
      const len = Math.sqrt(dx[i] * dx[i] + dy[i] * dy[i]) || 1;
      const move = Math.min(len, temperature);
      xs[i] += (dx[i] / len) * move;
      ys[i] += (dy[i] / len) * move;
    }
    temperature *= 0.985;
  }

  const at = new Map<number, { x: number; y: number }>();
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  graph.nodes.forEach((node, i) => {
    at.set(node.concept_id, { x: xs[i], y: ys[i] });
    minX = Math.min(minX, xs[i]);
    maxX = Math.max(maxX, xs[i]);
    minY = Math.min(minY, ys[i]);
    maxY = Math.max(maxY, ys[i]);
  });
  // Room for the largest circle and the label above it.
  const pad = 60;
  return {
    at,
    viewBox: `${minX - pad} ${minY - pad} ${maxX - minX + 2 * pad} ${maxY - minY + 2 * pad}`,
  };
}
