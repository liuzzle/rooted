import { useEffect, useMemo, useRef, useState } from "react";
import { LOW_CONFIDENCE, Page, Span, readPageImage } from "../../lib/api";

/**
 * Reviewing a scanned page.
 *
 * The page is the record; the recognised text is a reading of it. So the scan
 * stays on screen with every span drawn where it was found, and each span is
 * corrected in place rather than in a flattened block of prose. Nothing is
 * reordered or joined: a margin note stays in the margin, and what looks like a
 * list on the page stays a list of separate spans.
 *
 * Doubtful spans are marked, but a confident span can still be wrong — the
 * on-device engine reports full confidence for plain misreadings — so the
 * marking is a hint about where to look, never a filter on what needs reading.
 */
export default function ScanReview({
  pages,
  spans,
  edits,
  onEdit,
}: {
  pages: Page[];
  spans: Span[];
  edits: Map<number, string>;
  onEdit: (spanId: number, text: string) => void;
}) {
  const [pageIndex, setPageIndex] = useState(0);
  const [focused, setFocused] = useState<number | null>(null);
  const inputs = useRef(new Map<number, HTMLInputElement>());

  const page = pages[pageIndex] ?? null;
  const pageSpans = useMemo(
    () => spans.filter((s) => (page ? s.page_id === page.page_id : false)),
    [spans, page],
  );

  return (
    <div className="scan-review">
      <div className="scan-pane">
        {pages.length > 1 && (
          <div className="scan-pager">
            <button
              className="ghost-btn"
              disabled={pageIndex === 0}
              onClick={() => setPageIndex((i) => i - 1)}
            >
              ‹
            </button>
            <span className="card-note">
              Page {page?.page_no ?? 1} of {pages.length}
            </span>
            <button
              className="ghost-btn"
              disabled={pageIndex >= pages.length - 1}
              onClick={() => setPageIndex((i) => i + 1)}
            >
              ›
            </button>
          </div>
        )}
        {page && (
          <PageImage
            page={page}
            spans={pageSpans}
            edits={edits}
            focused={focused}
            onPick={(spanId) => {
              setFocused(spanId);
              inputs.current.get(spanId)?.focus();
              inputs.current.get(spanId)?.scrollIntoView({ block: "center" });
            }}
          />
        )}
      </div>

      <ol className="span-list">
        {pageSpans.map((span) => {
          const value = edits.get(span.span_id) ?? span.text;
          const doubtful =
            span.confidence != null && span.confidence < LOW_CONFIDENCE;
          const changed = value !== span.text;
          return (
            <li
              key={span.span_id}
              className={[
                "span-row",
                focused === span.span_id ? "focused" : "",
                doubtful ? "doubtful" : "",
                value.trim() === "" ? "dropped" : "",
              ]
                .filter(Boolean)
                .join(" ")}
              onMouseEnter={() => setFocused(span.span_id)}
            >
              <input
                ref={(el) => {
                  if (el) inputs.current.set(span.span_id, el);
                  else inputs.current.delete(span.span_id);
                }}
                value={value}
                spellCheck={false}
                onChange={(e) => onEdit(span.span_id, e.target.value)}
                onFocus={() => setFocused(span.span_id)}
              />
              <span className="span-meta">
                {span.confidence != null && `${Math.round(span.confidence * 100)}%`}
                {changed && <span className="span-edited"> edited</span>}
              </span>
            </li>
          );
        })}
        {pageSpans.length === 0 && (
          <p className="notes-empty">Nothing was recognised on this page.</p>
        )}
      </ol>
    </div>
  );
}

/** The scan with a box over every span, positioned from its stored geometry. */
function PageImage({
  page,
  spans,
  edits,
  focused,
  onPick,
}: {
  page: Page;
  spans: Span[];
  edits: Map<number, string>;
  focused: number | null;
  onPick: (spanId: number) => void;
}) {
  const [src, setSrc] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let url: string | null = null;
    let cancelled = false;
    readPageImage(page.page_id)
      .then((objectUrl) => {
        url = objectUrl;
        if (cancelled) URL.revokeObjectURL(objectUrl);
        else setSrc(objectUrl);
      })
      .catch((e) => setError(String(e)));
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [page.page_id]);

  if (error) return <p className="notes-error">Couldn't load the page: {error}</p>;

  return (
    <div className="scan-figure" style={{ aspectRatio: `${page.width} / ${page.height}` }}>
      {src ? <img src={src} alt={`Page ${page.page_no}`} /> : <div className="scan-loading" />}
      {spans.map((span) => {
        if (span.x == null || span.y == null || span.w == null || span.h == null)
          return null;
        const dropped = (edits.get(span.span_id) ?? span.text).trim() === "";
        const doubtful =
          span.confidence != null && span.confidence < LOW_CONFIDENCE;
        return (
          <button
            key={span.span_id}
            className={[
              "span-box",
              focused === span.span_id ? "focused" : "",
              doubtful ? "doubtful" : "",
              dropped ? "dropped" : "",
            ]
              .filter(Boolean)
              .join(" ")}
            style={{
              left: `${span.x * 100}%`,
              top: `${span.y * 100}%`,
              width: `${span.w * 100}%`,
              height: `${span.h * 100}%`,
            }}
            title={span.text}
            onClick={() => onPick(span.span_id)}
          />
        );
      })}
    </div>
  );
}
