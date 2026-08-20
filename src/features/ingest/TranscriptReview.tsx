import { EngineStatus, LOW_CONFIDENCE, Span } from "../../lib/api";

/**
 * Reviewing a transcript.
 *
 * Same contract as a scanned page, with time instead of space: every segment
 * keeps its own timestamps and speaker label, so a doubtful stretch can be
 * found in the recording and checked rather than trusted. Segments are never
 * merged into paragraphs — the timing is what makes the audio navigable.
 *
 * Speaker labels only appear when diarization actually ran. Without it the
 * transcript simply doesn't say who spoke, which is better than guessing at
 * speaker changes — and it says why, since "not installed" and "installed but
 * heard one voice" mean different things to the person reading.
 */
export default function TranscriptReview({
  spans,
  edits,
  onEdit,
  diarization,
}: {
  spans: Span[];
  edits: Map<number, string>;
  onEdit: (spanId: number, text: string) => void;
  diarization?: EngineStatus;
}) {
  const speakers = Array.from(
    new Set(spans.map((s) => s.speaker).filter(Boolean) as string[]),
  );

  return (
    <div className="transcript">
      {speakers.length > 0 ? (
        <p className="card-note">
          {speakers.length} speaker{speakers.length > 1 ? "s" : ""} detected:{" "}
          {speakers.join(", ")}
        </p>
      ) : diarization && !diarization.available ? (
        <p className="card-note">
          No speaker labels — {diarization.note}. The transcript is still
          correct; it just doesn't say who was talking.
        </p>
      ) : (
        <p className="card-note">
          No speaker labels on this recording — nothing was labelled rather than
          guessing at speaker changes.
        </p>
      )}

      <ol className="segment-list">
        {spans.map((span) => {
          const value = edits.get(span.span_id) ?? span.text;
          const doubtful =
            span.confidence != null && span.confidence < LOW_CONFIDENCE;
          return (
            <li
              key={span.span_id}
              className={[
                "segment",
                doubtful ? "doubtful" : "",
                value.trim() === "" ? "dropped" : "",
              ]
                .filter(Boolean)
                .join(" ")}
            >
              <div className="segment-meta">
                <span className="segment-time">
                  {formatTime(span.start_s)}–{formatTime(span.end_s)}
                </span>
                {span.speaker && (
                  <span className="segment-speaker">{span.speaker}</span>
                )}
                {span.confidence != null && (
                  <span className="segment-confidence">
                    {Math.round(span.confidence * 100)}%
                  </span>
                )}
              </div>
              <textarea
                rows={Math.max(1, Math.ceil(value.length / 90))}
                value={value}
                spellCheck={false}
                onChange={(e) => onEdit(span.span_id, e.target.value)}
              />
            </li>
          );
        })}
      </ol>
    </div>
  );
}

function formatTime(seconds: number | null): string {
  if (seconds == null) return "—";
  const total = Math.floor(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = `${m}`.padStart(h > 0 ? 2 : 1, "0");
  return `${h > 0 ? `${h}:` : ""}${mm}:${`${s}`.padStart(2, "0")}`;
}
