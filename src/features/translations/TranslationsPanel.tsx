import { useEffect, useState } from "react";
import {
  Pack,
  PackProgress,
  installPack,
  listPacks,
  onPackProgress,
  removePack,
} from "../../lib/api";

/**
 * Pack manager: download, switch to, and remove translations.
 *
 * Removing a pack deletes its text only — notes and highlights are user data
 * and stay put, so reinstalling brings them straight back.
 */
export default function TranslationsPanel({
  activeAbbrev,
  onUse,
  onInstalled,
  onClose,
}: {
  activeAbbrev: string | null;
  onUse: (abbrev: string) => void;
  onInstalled: () => void;
  onClose: () => void;
}) {
  const [packs, setPacks] = useState<Pack[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<PackProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  function refresh() {
    listPacks()
      .then(setPacks)
      .catch((e) => setError(String(e)));
  }

  useEffect(refresh, []);

  useEffect(() => {
    const unlisten = onPackProgress(setProgress);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  async function install(abbrev: string) {
    setBusy(abbrev);
    setError(null);
    setProgress(null);
    try {
      await installPack(abbrev);
      refresh();
      onInstalled();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
      setProgress(null);
    }
  }

  async function remove(pack: Pack) {
    setBusy(pack.abbrev);
    setError(null);
    try {
      await removePack(pack.abbrev);
      refresh();
      onInstalled();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <header className="modal-header">
          <h2>Translations</h2>
          <button className="icon-btn" onClick={onClose} title="Close">
            ×
          </button>
        </header>

        <p className="modal-note">
          Freely distributable texts only. Copyrighted translations aren't
          available here.
        </p>

        {error && <p className="notes-error">{error}</p>}

        <ul className="pack-list">
          {packs.map((p) => {
            const isBusy = busy === p.abbrev;
            const isActive = activeAbbrev === p.abbrev;
            return (
              <li key={p.abbrev} className={isActive ? "pack active" : "pack"}>
                <div className="pack-main">
                  <div className="pack-title">
                    <strong>{p.abbrev}</strong> <span>{p.name}</span>
                    {isActive && <span className="badge">reading</span>}
                  </div>
                  <div className="pack-blurb">{p.blurb}</div>
                  <div className="pack-meta">
                    {p.year} · {p.license}
                    {p.installed && ` · ${p.verse_count.toLocaleString()} verses`}
                  </div>
                  {isBusy && progress?.abbrev === p.abbrev && (
                    <div className="pack-progress">
                      <div className="bar">
                        <div
                          className="fill"
                          style={{
                            width: `${(progress.book / progress.total) * 100}%`,
                          }}
                        />
                      </div>
                      <span>
                        {progress.book_name} ({progress.book}/{progress.total}) ·{" "}
                        {progress.verses.toLocaleString()} verses
                      </span>
                    </div>
                  )}
                  {isBusy && !progress && <div className="pack-meta">Working…</div>}
                </div>

                <div className="pack-actions">
                  {p.installed ? (
                    <>
                      {!isActive && (
                        <button
                          className="primary"
                          disabled={busy !== null}
                          onClick={() => onUse(p.abbrev)}
                        >
                          Read
                        </button>
                      )}
                      <button
                        className="link-btn"
                        disabled={busy !== null}
                        onClick={() => remove(p)}
                      >
                        remove
                      </button>
                    </>
                  ) : (
                    <button
                      className="primary"
                      disabled={busy !== null}
                      onClick={() => install(p.abbrev)}
                    >
                      {isBusy ? "Downloading…" : "Download"}
                    </button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
}
