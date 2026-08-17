import { useEffect, useState } from "react";
import { JobDetail, getJob, verifyJob } from "../../lib/api";

/**
 * The human-in-the-loop step.
 *
 * The extracted text is shown exactly as the worker produced it, editable. What
 * you accept here is what becomes the note — nothing is added afterwards. This
 * is the gate the whole no-hallucination design rests on, so it is deliberately
 * unskippable: there is no "accept all" and no timer.
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
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getJob(jobId)
      .then((detail) => {
        setJob(detail);
        setText(detail.text ?? "");
      })
      .catch((e) => setError(String(e)));
  }, [jobId]);

  async function accept() {
    if (!text.trim()) return;
    setSaving(true);
    setError(null);
    try {
      await verifyJob(jobId, text);
      onDone();
    } catch (e) {
      setError(String(e));
      setSaving(false);
    }
  }

  if (error && !job) return <div className="empty-state">Error: {error}</div>;
  if (!job) return <div className="empty-state">Loading…</div>;

  const changed = text !== (job.text ?? "");
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
          </p>
        </div>
        <button className="icon-btn" onClick={onCancel} title="Back">
          ×
        </button>
      </header>

      {lowConfidence && (
        <p className="review-warning">
          The extraction wasn't certain — some characters may be wrong. Read it
          against the original before accepting.
        </p>
      )}

      <p className="field-hint">
        Correct anything that's wrong. What you accept is stored verbatim as the
        note; nothing is rewritten afterwards.
      </p>

      <textarea
        className="review-text"
        value={text}
        onChange={(e) => setText(e.target.value)}
        spellCheck={false}
      />

      {error && <p className="notes-error">{error}</p>}

      <footer className="review-actions">
        <button className="primary" onClick={accept} disabled={saving || !text.trim()}>
          {saving ? "Saving…" : changed ? "Accept corrected text" : "Accept as-is"}
        </button>
        <button className="link-btn" onClick={onCancel}>
          back
        </button>
        <span className="card-note">
          {text.trim().length.toLocaleString()} characters
          {changed && " · edited"}
        </span>
      </footer>
    </div>
  );
}
