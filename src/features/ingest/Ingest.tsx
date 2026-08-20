import { useCallback, useEffect, useRef, useState } from "react";
import {
  EngineStatus,
  Job,
  JobState,
  WorkerStatus,
  deleteJob,
  escalateJob,
  getWorkerStatus,
  listJobs,
  retryJob,
  uploadDocument,
} from "../../lib/api";
import ReviewPanel from "./ReviewPanel";

/** How the pipeline reads to a person, in order. */
const STAGES: { state: JobState; label: string; hint: string }[] = [
  { state: "UPLOADED", label: "Queued", hint: "waiting for the worker" },
  { state: "EXTRACTING", label: "Reading", hint: "pulling text out of the file" },
  { state: "NEEDS_REVIEW", label: "Needs review", hint: "waiting for you" },
  { state: "VERIFIED", label: "Accepted", hint: "becoming a note" },
  { state: "DONE", label: "Done", hint: "saved as a note" },
  { state: "ERROR", label: "Failed", hint: "retry or remove" },
];

const ACCEPT =
  ".txt,.md,.docx,.pdf,.jpg,.jpeg,.png,.heic,.tiff,.tif,.mp3,.m4a,.wav,.aiff,.flac";
const POLL_MS = 1500;

export default function Ingest({ onOpenNotes }: { onOpenNotes: () => void }) {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [worker, setWorker] = useState<WorkerStatus | null>(null);
  const [reviewing, setReviewing] = useState<number | null>(null);
  const [escalating, setEscalating] = useState<Job | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  const [title, setTitle] = useState("");
  const [docDate, setDocDate] = useState("");
  const [speaker, setSpeaker] = useState("");
  const [context, setContext] = useState("");

  const refresh = useCallback(() => {
    listJobs()
      .then(setJobs)
      .catch((e) => setError(String(e)));
    getWorkerStatus()
      .then(setWorker)
      .catch(() => {});
  }, []);

  // Jobs advance in another process, so poll while this view is open.
  useEffect(() => {
    refresh();
    const id = setInterval(refresh, POLL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  async function upload(files: FileList | null) {
    if (!files || files.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      for (const file of Array.from(files)) {
        const bytes = new Uint8Array(await file.arrayBuffer());
        await uploadDocument(file.name, bytes, {
          // With several files at once, per-file titles would be wrong.
          title: files.length === 1 ? title.trim() || null : null,
          doc_date: docDate.trim() || null,
          speaker: speaker.trim() || null,
          context: context.trim() || null,
        });
      }
      setTitle("");
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      if (fileInput.current) fileInput.current.value = "";
    }
  }

  const cloud = worker?.engines.find((e) => e.key === "cloud");
  const cloudReady = cloud?.available ?? false;

  const counts = new Map<JobState, number>();
  for (const job of jobs) counts.set(job.state, (counts.get(job.state) ?? 0) + 1);
  const needingReview = jobs.filter((j) => j.state === "NEEDS_REVIEW");

  if (reviewing != null) {
    return (
      <ReviewPanel
        jobId={reviewing}
        diarization={worker?.engines.find((e) => e.key === "diarization")}
        onDone={() => {
          setReviewing(null);
          refresh();
        }}
        onCancel={() => setReviewing(null)}
      />
    );
  }

  return (
    <div className="ingest">
      <header className="ingest-header">
        <div>
          <h2>Ingest</h2>
          <p className="card-note">
            Documents, scans and recordings. Scans are read on-device and come
            back as text you check on the page it came from; a PDF without a
            text layer is treated as a scan.
          </p>
        </div>
        <WorkerBadge status={worker} />
      </header>

      {error && <p className="notes-error">{error}</p>}

      {escalating && (
        <EscalateConfirm
          job={escalating}
          onCancel={() => setEscalating(null)}
          onConfirm={() => {
            const job = escalating;
            setEscalating(null);
            escalateJob(job.job_id)
              .then(refresh)
              .catch((e) => setError(String(e)));
          }}
        />
      )}

      <EngineList engines={worker?.engines ?? []} />

      <section className="card upload">
        <h3>Add documents</h3>
        <div className="upload-meta">
          <label>
            Title
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Defaults to the filename"
            />
          </label>
          <label>
            Date
            <input
              type="date"
              value={docDate}
              onChange={(e) => setDocDate(e.target.value)}
            />
          </label>
          <label>
            Speaker
            <input
              value={speaker}
              onChange={(e) => setSpeaker(e.target.value)}
              placeholder="Who gave the talk"
            />
          </label>
          <label className="wide">
            Context
            <input
              value={context}
              onChange={(e) => setContext(e.target.value)}
              placeholder="Where these notes came from"
            />
          </label>
        </div>
        <div className="upload-actions">
          <input
            ref={fileInput}
            type="file"
            accept={ACCEPT}
            multiple
            disabled={busy}
            onChange={(e) => upload(e.target.files)}
          />
          {busy && <span className="card-note">Uploading…</span>}
        </div>
        <p className="field-hint">
          This metadata is attached to every note the upload produces, and
          travels with it into citations.
        </p>
      </section>

      <section className="pipeline">
        {STAGES.map((stage) => (
          <div
            key={stage.state}
            className={
              (counts.get(stage.state) ?? 0) > 0 ? "stage populated" : "stage"
            }
            title={stage.hint}
          >
            <span className="stage-count">{counts.get(stage.state) ?? 0}</span>
            <span className="stage-label">{stage.label}</span>
          </div>
        ))}
      </section>

      {needingReview.length > 0 && (
        <p className="review-callout">
          {needingReview.length} document
          {needingReview.length > 1 ? "s are" : " is"} waiting for you to check
          the text. Nothing becomes a note until you do.
        </p>
      )}

      <ul className="job-list">
        {jobs.length === 0 && (
          <p className="notes-empty">
            Nothing ingested yet. Add a document above.
          </p>
        )}
        {jobs.map((job) => (
          <li key={job.job_id} className={`job ${job.state.toLowerCase()}`}>
            <div className="job-head">
              <span className="job-name">{job.title ?? job.filename}</span>
              <StateChip job={job} />
            </div>
            <div className="job-meta">
              {job.filename} · {formatBytes(job.byte_size)}
              {job.speaker && ` · ${job.speaker}`}
              {job.doc_date && ` · ${job.doc_date}`}
              {job.engine_used && ` · ${job.engine_used}`}
              {job.confidence != null &&
                ` · ${Math.round(job.confidence * 100)}% confident`}
              {job.edited && " · corrected"}
            </div>

            {job.last_error && <p className="job-error">{job.last_error}</p>}
            {job.preview && <p className="job-preview">{job.preview}</p>}

            <div className="note-actions">
              {job.state === "NEEDS_REVIEW" && (
                <button
                  className="primary"
                  onClick={() => setReviewing(job.job_id)}
                >
                  Review text
                </button>
              )}
              {job.state === "DONE" && (
                <button className="link-btn" onClick={onOpenNotes}>
                  see the note
                </button>
              )}
              {(job.state === "ERROR" || job.state === "NEEDS_REVIEW") && (
                <button
                  className="link-btn"
                  onClick={() =>
                    retryJob(job.job_id).then(refresh).catch((e) => setError(String(e)))
                  }
                >
                  read the file again
                </button>
              )}
              {cloudReady && (job.state === "ERROR" || job.state === "NEEDS_REVIEW") && (
                <button className="link-btn" onClick={() => setEscalating(job)}>
                  read it in the cloud
                </button>
              )}
              <button
                className="link-btn"
                onClick={() =>
                  deleteJob(job.job_id).then(refresh).catch((e) => setError(String(e)))
                }
              >
                remove
              </button>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

function StateChip({ job }: { job: Job }) {
  const stage = STAGES.find((s) => s.state === job.state);
  return (
    <span className={`state-chip ${job.state.toLowerCase()}`}>
      {stage?.label ?? job.state}
    </span>
  );
}

/** The worker runs as its own process; say so plainly when it isn't there. */
function WorkerBadge({ status }: { status: WorkerStatus | null }) {
  if (!status) return null;
  if (status.running) {
    return (
      <span className="worker-badge ok" title={`last seen ${status.last_heartbeat ?? "—"}`}>
        Worker running
      </span>
    );
  }
  return (
    <span className="worker-badge down" title={status.problem ?? undefined}>
      Worker not running — uploads will queue
    </span>
  );
}

/**
 * The one place the app asks before doing something it can't undo.
 *
 * Everything else in Rooted happens on this machine. This doesn't, so it says
 * exactly what travels — cropped lines from this page, nothing else — and what
 * comes back is still machine text that has to be read in review. No "don't
 * ask again": it is a per-page decision, and that's the point.
 */
function EscalateConfirm({
  job,
  onConfirm,
  onCancel,
}: {
  job: Job;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <section className="card escalate-confirm">
      <h3>Send this page to be read?</h3>
      <p className="card-note">
        The lines this machine found on <strong>{job.title ?? job.filename}</strong>{" "}
        will be cropped and sent to Anthropic to be read again. The note, its
        date and speaker, and every other document stay here.
      </p>
      <p className="card-note">
        What comes back is another machine reading — you still check it in
        review before it becomes a note.
      </p>
      <div className="note-actions">
        <button className="primary" onClick={onConfirm}>
          Send the lines
        </button>
        <button className="link-btn" onClick={onCancel}>
          keep it on this machine
        </button>
      </div>
    </section>
  );
}

/**
 * What this machine can read, before anything is uploaded.
 *
 * An engine that isn't installed is a fact about the machine, not a failure of
 * the file — so it belongs here rather than on a job that has already failed.
 * The worker supplies the text, including how to install what's missing.
 */
function EngineList({ engines }: { engines: EngineStatus[] }) {
  if (engines.length === 0) return null;
  const missing = engines.filter((e) => !e.available);
  return (
    <section className="engines">
      <ul className="engine-row">
        {engines.map((engine) => (
          <li
            key={engine.key}
            className={`engine ${engine.available ? "on" : "off"}`}
            title={`${engine.engine} — ${engine.note}`}
          >
            {engine.label}
          </li>
        ))}
      </ul>
      {missing.map((engine) => (
        <p key={engine.key} className="card-note engine-missing">
          <strong>{engine.label}</strong> unavailable — {engine.note}
        </p>
      ))}
    </section>
  );
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} kB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
