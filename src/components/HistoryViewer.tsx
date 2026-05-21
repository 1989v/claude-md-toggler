import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type HistoryEntry = {
  id: number;
  ts: string;
  action: string;
  from_name: string | null;
  to_name: string | null;
  ok: boolean;
  error: string | null;
};

const ACTION_LABEL: Record<string, string> = {
  toggle: "Toggle",
  "drift-apply-to-active": "Drift → profile",
  "drift-apply-to-origin": "Drift → origin",
  "drift-discard": "Drift discard",
};

function formatTs(iso: string): string {
  // Render the ISO timestamp as locale time. Keep the date short — the
  // viewer is a glance-at-recent-activity, not an audit log UI.
  try {
    const d = new Date(iso);
    return d.toLocaleString();
  } catch {
    return iso;
  }
}

export default function HistoryViewer(props: { onClose: () => void }) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    (async () => {
      try {
        const rows = await invoke<HistoryEntry[]>("list_history", {
          limit: 200,
        });
        setEntries(rows);
      } catch (e) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  return (
    <main className="app history">
      <header>
        <h1>Toggle history</h1>
        <p className="subtitle">{entries.length} entries (most recent first)</p>
      </header>

      {error && <div className="error">{error}</div>}

      {loading ? (
        <p className="hint">Loading…</p>
      ) : entries.length === 0 ? (
        <p className="hint">No toggle events recorded yet.</p>
      ) : (
        <ul className="history-list">
          {entries.map((e) => (
            <li key={e.id} className={e.ok ? "" : "failed"}>
              <div className="row-1">
                <span className="action">
                  {ACTION_LABEL[e.action] ?? e.action}
                </span>
                <span className="ts">{formatTs(e.ts)}</span>
              </div>
              <div className="row-2">
                <span className="profiles">
                  {e.from_name && (
                    <>
                      <code>{e.from_name}</code> →{" "}
                    </>
                  )}
                  <code>{e.to_name ?? "—"}</code>
                </span>
                {!e.ok && e.error && (
                  <span className="err-msg" title={e.error}>
                    {e.error}
                  </span>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}

      <footer className="actions">
        <button onClick={props.onClose}>Close</button>
      </footer>
    </main>
  );
}
