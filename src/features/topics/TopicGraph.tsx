import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ConceptGraph,
  GraphEdge,
  GraphNode,
  SharedPassage,
  conceptGraph,
  sharedPassages,
} from "../../lib/api";
import Marked from "../notes/Marked";
import { Sim, bounds, createSim, pin, position, settle, step, unpin } from "./forceLayout";

/**
 * The topic graph.
 *
 * A line joins two topics only where one passage of one note mentions both,
 * and clicking it shows those passages. So the graph claims nothing a note
 * doesn't say: "these were written together, here" is the whole meaning of an
 * edge. Similarity is deliberately not drawn — it is a guess, and a line on a
 * graph looks like a fact.
 *
 * Interaction: drag a topic and its neighbours follow; drag the background to
 * pan; scroll to zoom. Click selects (a topic, or a line), double-click opens
 * the topic page.
 */

/** Pointer travel, in screen pixels, below which a press is a click. */
const CLICK_SLOP = 4;
/** Two clicks on one topic this close together open it. Detected here, not by
 *  `dblclick`: pointer capture retargets that event to the canvas. */
const DOUBLE_CLICK_MS = 350;
/** Topics labelled even when zoomed out; the rest appear as you zoom in. */
const ALWAYS_LABELLED = 30;
const MIN_ZOOM = 0.3;
const MAX_ZOOM = 6;

interface View {
  tx: number;
  ty: number;
  s: number;
}

type Press =
  | { kind: "node"; id: number; x0: number; y0: number; moved: boolean }
  | { kind: "pan"; edge: GraphEdge | null; x0: number; y0: number; view: View; moved: boolean };

export default function TopicGraph({
  onOpenTopic,
  onOpenNote,
}: {
  onOpenTopic: (conceptId: number) => void;
  onOpenNote: (noteId: number) => void;
}) {
  const [graph, setGraph] = useState<ConceptGraph | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    conceptGraph(150)
      .then(setGraph)
      .catch((e) => setError(String(e)));
  }, []);

  if (error) return <p className="notes-error">{error}</p>;
  if (!graph) return null;
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
  return <GraphCanvas graph={graph} onOpenTopic={onOpenTopic} onOpenNote={onOpenNote} />;
}

function GraphCanvas({
  graph,
  onOpenTopic,
  onOpenNote,
}: {
  graph: ConceptGraph;
  onOpenTopic: (conceptId: number) => void;
  onOpenNote: (noteId: number) => void;
}) {
  // The simulation is mutable and lives outside React; `frame` re-renders it.
  const sim = useMemo<Sim>(() => {
    const s = createSim(
      graph.nodes.map((n) => n.concept_id),
      graph.edges,
    );
    settle(s);
    return s;
  }, [graph]);
  const [, setFrame] = useState(0);
  const [base, setBase] = useState(() => bounds(sim));
  const [view, setView] = useState<View>({ tx: 0, ty: 0, s: 1 });
  const [hover, setHover] = useState<number | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [edge, setEdge] = useState<GraphEdge | null>(null);
  const [passages, setPassages] = useState<SharedPassage[]>([]);
  const [find, setFind] = useState("");

  const svgRef = useRef<SVGSVGElement>(null);
  const layerRef = useRef<SVGGElement>(null);
  const press = useRef<Press | null>(null);
  const heat = useRef(0);
  const raf = useRef<number | null>(null);
  const lastClick = useRef<{ id: number; at: number } | null>(null);
  const viewRef = useRef(view);
  viewRef.current = view;
  // Graph units per screen pixel at zoom 1, so circles and labels can be
  // sized for the eye rather than for the layout's coordinate space.
  const [upp, setUpp] = useState(1);

  useEffect(() => {
    const svg = svgRef.current;
    if (!svg) return;
    const measure = () => {
      const { width, height } = svg.getBoundingClientRect();
      if (width > 0 && height > 0) {
        // The viewBox is fitted with "meet": the tighter axis decides.
        setUpp(Math.max(base.w / width, base.h / height));
      }
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(svg);
    return () => observer.disconnect();
  }, [base]);

  const byId = useMemo(
    () => new Map(graph.nodes.map((n) => [n.concept_id, n])),
    [graph],
  );
  const neighbours = useMemo(() => {
    const out = new Map<number, { id: number; passages: number }[]>();
    for (const e of graph.edges) {
      for (const [from, to] of [
        [e.a, e.b],
        [e.b, e.a],
      ]) {
        const list = out.get(from) ?? [];
        list.push({ id: to, passages: e.passages });
        out.set(from, list);
      }
    }
    for (const list of out.values()) list.sort((x, y) => y.passages - x.passages);
    return out;
  }, [graph]);

  useEffect(() => {
    if (!edge) return;
    setPassages([]);
    sharedPassages(edge.a, edge.b)
      .then(setPassages)
      .catch(() => setPassages([]));
  }, [edge]);

  // --- the live simulation ---------------------------------------------------

  const tick = useCallback(() => {
    const dragging = press.current?.kind === "node" && press.current.moved;
    step(sim, heat.current);
    // Held warm while dragging, so neighbours keep following; cools after.
    heat.current = dragging ? Math.max(heat.current, sim.k * 0.25) : heat.current * 0.94;
    setFrame((f) => f + 1);
    if (dragging || heat.current > 0.3) {
      raf.current = requestAnimationFrame(tick);
    } else {
      raf.current = null;
    }
  }, [sim]);

  const reheat = useCallback(
    (amount: number) => {
      heat.current = Math.max(heat.current, amount);
      if (raf.current == null) raf.current = requestAnimationFrame(tick);
    },
    [tick],
  );

  useEffect(
    () => () => {
      if (raf.current != null) cancelAnimationFrame(raf.current);
    },
    [],
  );

  // --- coordinates -----------------------------------------------------------

  /** Screen point → graph coordinates (inside pan and zoom). */
  function toGraph(clientX: number, clientY: number) {
    const layer = layerRef.current;
    const svg = svgRef.current;
    const ctm = layer?.getScreenCTM();
    if (!svg || !ctm) return null;
    const p = svg.createSVGPoint();
    p.x = clientX;
    p.y = clientY;
    return p.matrixTransform(ctm.inverse());
  }

  /** Screen point → viewBox coordinates (outside pan and zoom). */
  function toViewBox(clientX: number, clientY: number) {
    const svg = svgRef.current;
    const ctm = svg?.getScreenCTM();
    if (!svg || !ctm) return null;
    const p = svg.createSVGPoint();
    p.x = clientX;
    p.y = clientY;
    return p.matrixTransform(ctm.inverse());
  }

  /** viewBox units per screen pixel. */
  function unitsPerPixel() {
    const ctm = svgRef.current?.getScreenCTM();
    return ctm ? 1 / ctm.a : 1;
  }

  // Wheel zoom needs a non-passive listener to stop the page scrolling, which
  // React's onWheel can't provide.
  useEffect(() => {
    const svg = svgRef.current;
    if (!svg) return;
    function onWheel(e: WheelEvent) {
      e.preventDefault();
      const at = toViewBox(e.clientX, e.clientY);
      if (!at) return;
      const v = viewRef.current;
      // Trackpads send small deltas, mice large ones; exp() treats both alike.
      const s = clamp(v.s * Math.exp(-e.deltaY * 0.0015), MIN_ZOOM, MAX_ZOOM);
      const r = s / v.s;
      setView({ s, tx: at.x - (at.x - v.tx) * r, ty: at.y - (at.y - v.ty) * r });
    }
    svg.addEventListener("wheel", onWheel, { passive: false });
    return () => svg.removeEventListener("wheel", onWheel);
  }, []);

  // --- pointer ---------------------------------------------------------------

  function onPointerDown(e: React.PointerEvent<SVGSVGElement>) {
    if (e.button !== 0) return;
    const target = e.target as Element;
    const node = target.closest("[data-node]");
    const line = target.closest("[data-edge]");
    svgRef.current?.setPointerCapture(e.pointerId);
    if (node) {
      press.current = {
        kind: "node",
        id: Number(node.getAttribute("data-node")),
        x0: e.clientX,
        y0: e.clientY,
        moved: false,
      };
    } else {
      const [a, b] = (line?.getAttribute("data-edge") ?? "").split("-").map(Number);
      press.current = {
        kind: "pan",
        edge: line ? graph.edges.find((x) => x.a === a && x.b === b) ?? null : null,
        x0: e.clientX,
        y0: e.clientY,
        view: viewRef.current,
        moved: false,
      };
    }
  }

  function onPointerMove(e: React.PointerEvent<SVGSVGElement>) {
    const p = press.current;
    if (!p) return;
    const far = Math.hypot(e.clientX - p.x0, e.clientY - p.y0) > CLICK_SLOP;
    if (!p.moved && !far) return;
    p.moved = true;
    if (p.kind === "node") {
      const at = toGraph(e.clientX, e.clientY);
      if (!at) return;
      pin(sim, p.id, at.x, at.y);
      reheat(sim.k * 0.25);
    } else {
      const u = unitsPerPixel();
      setView({
        s: p.view.s,
        tx: p.view.tx + (e.clientX - p.x0) * u,
        ty: p.view.ty + (e.clientY - p.y0) * u,
      });
    }
  }

  function onPointerUp() {
    const p = press.current;
    press.current = null;
    if (!p) return;
    if (p.kind === "node") {
      unpin(sim);
      if (p.moved) {
        reheat(sim.k * 0.15);
        return;
      }
      const now = performance.now();
      const prev = lastClick.current;
      if (prev && prev.id === p.id && now - prev.at < DOUBLE_CLICK_MS) {
        lastClick.current = null;
        onOpenTopic(p.id);
      } else {
        lastClick.current = { id: p.id, at: now };
        selectTopic(p.id);
      }
    } else if (!p.moved) {
      if (p.edge) selectEdge(p.edge);
      else {
        setSelected(null);
        setEdge(null);
      }
    }
  }

  // --- selection and view ----------------------------------------------------

  function selectTopic(id: number) {
    setSelected(id);
    setEdge(null);
  }

  function selectEdge(e: GraphEdge) {
    setEdge(e);
    setSelected(null);
  }

  function edgeBetween(a: number, b: number) {
    return graph.edges.find((e) => (e.a === a && e.b === b) || (e.a === b && e.b === a));
  }

  /** Put a topic in the middle of the canvas, without changing the zoom. */
  function centreOn(id: number) {
    const at = position(sim, id);
    if (!at) return;
    const cx = base.x + base.w / 2;
    const cy = base.y + base.h / 2;
    setView((v) => ({ s: v.s, tx: cx - v.s * at.x, ty: cy - v.s * at.y }));
  }

  function resetView() {
    setBase(bounds(sim));
    setView({ tx: 0, ty: 0, s: 1 });
  }

  function onFind(text: string) {
    setFind(text);
    const want = text.trim().toLocaleLowerCase();
    if (!want) return;
    const hit =
      graph.nodes.find((n) => n.label.toLocaleLowerCase() === want) ??
      graph.nodes.find((n) => n.label.toLocaleLowerCase().startsWith(want)) ??
      graph.nodes.find((n) => n.label.toLocaleLowerCase().includes(want));
    if (hit) {
      selectTopic(hit.concept_id);
      centreOn(hit.concept_id);
    }
  }

  // --- drawing ---------------------------------------------------------------

  /** Graph units per screen pixel, at the current zoom. */
  const px = upp / view.s;
  const focus = hover ?? selected;
  const near = new Set<number>();
  if (focus != null) for (const n of neighbours.get(focus) ?? []) near.add(n.id);
  const lit = (id: number) => focus == null || id === focus || near.has(id);
  const rank = new Map(graph.nodes.map((n, i) => [n.concept_id, i]));
  const labelled = (id: number) =>
    (rank.get(id) ?? 0) < ALWAYS_LABELLED * view.s || id === focus || near.has(id);

  const chosen = selected != null ? byId.get(selected) : null;

  return (
    <div className="topic-graph">
      <div className="graph-stage">
        <div className="graph-toolbar">
          <input
            className="graph-find"
            value={find}
            onChange={(e) => onFind(e.target.value)}
            placeholder="Find a topic"
            list="graph-topics"
          />
          <datalist id="graph-topics">
            {graph.nodes.map((n) => (
              <option key={n.concept_id} value={n.label} />
            ))}
          </datalist>
          <button className="ghost-btn" onClick={resetView} title="Fit the whole graph">
            Reset view
          </button>
        </div>
        <svg
          ref={svgRef}
          className={press.current?.kind === "pan" && press.current.moved ? "graph-canvas panning" : "graph-canvas"}
          viewBox={`${base.x} ${base.y} ${base.w} ${base.h}`}
          role="img"
          aria-label="Topics, joined where a passage mentions both"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
        >
          <g ref={layerRef} transform={`translate(${view.tx},${view.ty}) scale(${view.s})`}>
            {graph.edges.map((e) => {
              const a = position(sim, e.a)!;
              const b = position(sim, e.b)!;
              const on = focus == null || e.a === focus || e.b === focus;
              const isChosen = edge && edge.a === e.a && edge.b === e.b;
              return (
                <g
                  key={`${e.a}-${e.b}`}
                  data-edge={`${e.a}-${e.b}`}
                  className={isChosen ? "graph-edge chosen" : "graph-edge"}
                  opacity={on ? 1 : 0.12}
                >
                  <line
                    x1={a.x}
                    y1={a.y}
                    x2={b.x}
                    y2={b.y}
                    className="graph-edge-hit"
                    strokeWidth={10 * px}
                  />
                  <line
                    x1={a.x}
                    y1={a.y}
                    x2={b.x}
                    y2={b.y}
                    strokeWidth={(1 + Math.log2(e.passages)) * px}
                  />
                  <title>
                    {byId.get(e.a)?.label} + {byId.get(e.b)?.label}: {e.passages} passage
                    {e.passages === 1 ? "" : "s"}
                  </title>
                </g>
              );
            })}
            {graph.nodes.map((n) => {
              const p = position(sim, n.concept_id)!;
              const r = radius(n) * px;
              const isSelected = n.concept_id === selected;
              return (
                <g
                  key={n.concept_id}
                  data-node={n.concept_id}
                  className={isSelected ? "graph-node selected" : "graph-node"}
                  transform={`translate(${p.x},${p.y})`}
                  opacity={lit(n.concept_id) ? 1 : 0.2}
                  onPointerEnter={() => setHover(n.concept_id)}
                  onPointerLeave={() => setHover(null)}
                >
                  <circle r={r} />
                  {labelled(n.concept_id) && (
                    <text
                      y={-r - 4 * px}
                      fontSize={12 * px}
                      strokeWidth={3 * px}
                    >
                      {n.label}
                    </text>
                  )}
                </g>
              );
            })}
          </g>
        </svg>
        <p className="graph-hint">
          Drag a topic to move it · drag the background to pan · scroll to zoom ·
          double-click a topic to open it
        </p>
      </div>

      <aside className="graph-side">
        {!chosen && !edge && (
          <p className="card-note">
            Click a topic to see what it's written alongside. Click a line to
            read the passages that mention both — a line exists only because
            one does.
          </p>
        )}

        {chosen && (
          <>
            <h3>{chosen.label}</h3>
            <p className="card-note">
              In {chosen.notes} note{chosen.notes === 1 ? "" : "s"}.{" "}
              <button className="link-btn" onClick={() => onOpenTopic(chosen.concept_id)}>
                Open topic page
              </button>
            </p>
            <h4 className="graph-side-sub">Written alongside</h4>
            <ul className="graph-neighbours">
              {(neighbours.get(chosen.concept_id) ?? []).map((nb) => (
                <li key={nb.id}>
                  <button
                    className="link-btn"
                    onClick={() => {
                      const e = edgeBetween(chosen.concept_id, nb.id);
                      if (e) selectEdge(e);
                    }}
                    onMouseEnter={() => setHover(nb.id)}
                    onMouseLeave={() => setHover(null)}
                  >
                    {byId.get(nb.id)?.label}
                  </button>
                  <span className="topic-count">
                    {" "}
                    {nb.passages} passage{nb.passages === 1 ? "" : "s"}
                  </span>
                </li>
              ))}
              {(neighbours.get(chosen.concept_id) ?? []).length === 0 && (
                <li className="card-note">Nothing yet — no passage mentions it with another topic.</li>
              )}
            </ul>
          </>
        )}

        {edge && (
          <>
            <h3>
              <button className="link-btn" onClick={() => selectTopic(edge.a)}>
                {byId.get(edge.a)?.label}
              </button>{" "}
              ·{" "}
              <button className="link-btn" onClick={() => selectTopic(edge.b)}>
                {byId.get(edge.b)?.label}
              </button>
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

/** Screen pixels: bigger for topics more notes reach. */
function radius(n: GraphNode) {
  return 4 + 2.5 * Math.sqrt(n.notes);
}

function clamp(x: number, lo: number, hi: number) {
  return Math.min(hi, Math.max(lo, x));
}
