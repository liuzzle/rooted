import { useEffect, useMemo, useState } from "react";
import {
  JobDetail,
  LOW_CONFIDENCE,
  getJob,
  verifyJob,
  verifyJobSpans,
} from "../../lib/api";
import ScanReview from "./ScanReview";
import TranscriptReview from "./TranscriptReview";

/**
 * The human-in-the-loop step.
 *
 * Whatever the source, the same rule holds: what you accept here is what
 * becomes the note, stored verbatim, and nothing is added afterwards. Typed
 * documents are one editable block; a scan is its page with spans in place; a
 * recording is its timed segments. The gate is deliberately unskippable —
 * there is no "accept all" across jobs and no timer.
 */
export default function ReviewPanel({
  jobId,
  onDone,
  onCancel,
}: {
  jobId: number;
  onDone: () => void;
  onCancel: () => void;
}) {
  const [job, setJob] = useState<JobDetail | null>(null);
  const [text, setText] = useState("");
  const [edits, setEdits] = useState<Map<number, string>>(new Map());
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getJob(jobId)
      .then((detail) => {
        setJob(detail);
        setText(detail.text ?? "");
        setEdits(new Map());
      })
      .catch((e) => setError(String(e)));
  }, [jobId]);

  function editSpan(spanId: number, value: string) {
    setEdits((current) => new Map(current).set(spanId, value));
  }

  const doubtful = useMemo(
    () =>
      job?.spans.filter(
        (s) => s.confidence != null && s.confidence < LOW_CONFIDENCE,
      ).length ?? 0,
    [job],
  );

  const changed = useMemo(() => {
    if (!job) return false;
    if (job.kind === "typed") return text !== (job.text ?? "");
    return job.spans.some((s) => (edits.get(s.span_id) ?? s.text) !== s.text);
  }, [job, text, edits]);

  const empty = useMemo(() => {
    if (!job) return true;
    if (job.kind === "typed") return !text.trim();
    return job.spans.every((s) => (edits.get(s.span_id) ?? s.text).trim() === "");
  }, [job, text, edits]);

  async function accept() {
    if (!job || empty) return;
    setSaving(true);
    setError(null);
    try {
      if (job.kind === "typed") {
        await verifyJob(jobId, text);
      } else {
        await verifyJobSpans(
          jobId,
          job.spans.map((s) => ({
            span_id: s.span_id,
            text: edits.get(s.span_id) ?? s.text,
          })),
        );
      }
      onDone();
    } catch (e) {
      setError(String(e));
      setSaving(false);
    }
  }

  if (error && !job) return <div className="empty-state">Error: {error}</div>;
  if (!job) return <div className="empty-state">Loading…</div>;

  const lowConfidence = job.confidence != null && job.confidence < 1;

  return (
    <div className="review">
      <header className="review-header">
        <div>
          <span className="notes-kind">Review</span>
          <h2>{job.title ?? job.filename}</h2>
          <p className="card-note">
            {job.filename} · {job.engine_used ?? "—"}
            {job.confidence != null &&
              ` · ${Math.round(job.confidence * 100)}% confident`}
            {job.speaker && ` · ${job.speaker}`}
            {job.doc_date && ` · ${job.doc_date}`}
            {job.pages.length > 0 &&
              ` · ${job.pages.length} page${job.pages.length > 1 ? "s" : ""}`}
            {job.spans.length > 0 && ` · ${job.spans.length} spans`}
          </p>
        </div>
        <button className="icon-btn" onClick={onCancel} title="Back">
          ×
        </button>
      </header>

      {job.kind !== "typed" && (
        <p className="review-warning">
          {doubtful > 0
            ? `${doubtful} of ${job.spans.length} spans were read with low confidence and are marked. `
            : ""}
          Read this against the original before accepting — a confident reading
          can still be wrong.
        </p>
      )}
      {job.kind === "typed" && lowConfidence && (
        <p className="review-warning">
          The extraction wasn't certain — some characters may be wrong. Read it
          against the original before accepting.
        </p>
      )}

      <p className="field-hint">
        {job.kind === "image"
          ? "Correct each line where it sits on the page. Clear a line to drop it — a stray mark the engine read as a word."
          : job.kind === "audio"
            ? "Correct each segment against the recording. Timestamps and speakers are kept."
            : "Correct anything that's wrong. What you accept is stored verbatim as the note; nothing is rewritten afterwards."}
      </p>

      {job.kind === "typed" && (
        <textarea
          className="review-text"
          value={text}
          onChange={(e) => setText(e.target.value)}
          spellCheck={false}
        />
      )}
      {job.kind === "image" && (
        <ScanReview
          pages={job.pages}
          spans={job.spans}
          edits={edits}
          onEdit={editSpan}
        />
      )}
      {job.kind === "audio" && (
        <TranscriptReview spans={job.spans} edits={edits} onEdit={editSpan} />
      )}

      {error && <p className="notes-error">{error}</p>}

      <footer className="review-actions">
        <button className="primary" onClick={accept} disabled={saving || empty}>
          {saving ? "Saving…" : changed ? "Accept corrected text" : "Accept as-is"}
        </button>
        <button className="link-btn" onClick={onCancel}>
          back
        </button>
        <span className="card-note">
          {job.kind === "typed"
            ? `${text.trim().length.toLocaleString()} characters`
            : `${job.spans.length} spans`}
          {changed && " · edited"}
        </span>
      </footer>
    </div>
  );
}
