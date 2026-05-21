import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import ProfileEditor, { type EditorTarget } from "./components/ProfileEditor";
import ConflictDialog, {
  type DriftChoice,
  type DriftInfo,
} from "./components/ConflictDialog";
import HistoryViewer from "./components/HistoryViewer";

export type ProfileSummary = {
  name: string;
  path: string;
  is_active: boolean;
};

function App() {
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [active, setActive] = useState<string>("origin");
  const [hasDrift, setHasDrift] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<EditorTarget | null>(null);
  const [drift, setDrift] = useState<{
    info: DriftInfo;
    pending: string;
  } | null>(null);
  const [showHistory, setShowHistory] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [list, current, driftInfo] = await Promise.all([
        invoke<ProfileSummary[]>("list_profiles"),
        invoke<string>("get_active_profile"),
        invoke<DriftInfo | null>("check_drift"),
      ]);
      setProfiles(list);
      setActive(current);
      setHasDrift(driftInfo !== null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Listen for external edits to CLAUDE.md. The backend emits this whenever
  // the file is modified on disk (debounced); we just re-query state so the
  // drift indicator stays current.
  useEffect(() => {
    let unlisten: UnlistenFn | null = null;
    (async () => {
      unlisten = await listen("claude-md:changed", () => {
        refresh();
      });
    })();
    return () => {
      unlisten?.();
    };
  }, [refresh]);

  const safeInvoke = useCallback(
    async (fn: () => Promise<unknown>) => {
      try {
        await fn();
        await refresh();
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh],
  );

  const onToggle = useCallback(
    async (name: string) => {
      try {
        const driftInfo = await invoke<DriftInfo | null>("check_drift");
        if (driftInfo) {
          setDrift({ info: driftInfo, pending: name });
          return;
        }
        await invoke("toggle_profile", { name });
        await refresh();
      } catch (e) {
        setError(String(e));
      }
    },
    [refresh],
  );

  const onDriftResolved = useCallback(
    async (choice: DriftChoice) => {
      const pending = drift?.pending;
      setDrift(null);
      if (choice === "cancel" || !pending) {
        await refresh();
        return;
      }
      try {
        await invoke("toggle_profile", { name: pending });
      } catch (e) {
        setError(String(e));
      } finally {
        await refresh();
      }
    },
    [drift, refresh],
  );

  const onDelete = (name: string) => {
    if (!confirm(`Delete profile "${name}"? This cannot be undone.`)) return;
    safeInvoke(() => invoke("delete_profile", { name }));
  };

  const onRename = (name: string) => {
    const next = prompt(`Rename "${name}" to:`, name);
    if (!next || next === name) return;
    safeInvoke(() =>
      invoke("rename_profile", { oldName: name, newName: next }),
    );
  };

  const onDuplicate = (name: string) => {
    const next = prompt(`Duplicate "${name}" as:`, `${name}-copy`);
    if (!next) return;
    safeInvoke(() =>
      invoke("duplicate_profile", { source: name, newName: next }),
    );
  };

  if (editor) {
    return (
      <ProfileEditor
        target={editor}
        onClose={() => setEditor(null)}
        onSaved={async () => {
          setEditor(null);
          await refresh();
        }}
      />
    );
  }

  if (showHistory) {
    return <HistoryViewer onClose={() => setShowHistory(false)} />;
  }

  return (
    <>
      {drift && (
        <ConflictDialog
          drift={drift.info}
          pendingProfile={drift.pending}
          onResolved={onDriftResolved}
        />
      )}

      <main className="app">
        <header>
          <h1>Claude.md Toggler</h1>
          <p className="subtitle">
            Active: <strong>{active}</strong>
            {hasDrift && (
              <span className="drift-badge" title="CLAUDE.md was edited outside the app">
                edited
              </span>
            )}
          </p>
        </header>

        {error && <div className="error">{error}</div>}

        <section className="profiles">
          <div className="section-head">
            <h2>Profiles</h2>
            <button className="ghost" onClick={() => setEditor({ mode: "new" })}>
              + New
            </button>
          </div>

          <ul>
            <ProfileRow
              name="origin"
              isActive={active === "origin"}
              onToggle={() => onToggle("origin")}
              onEdit={() =>
                setEditor({ mode: "edit", name: "origin", readOnly: true })
              }
              onDuplicate={() => onDuplicate("origin")}
              reserved
            />
            {profiles
              .filter((p) => p.name !== "origin")
              .map((p) => (
                <ProfileRow
                  key={p.name}
                  name={p.name}
                  isActive={p.is_active}
                  onToggle={() => onToggle(p.name)}
                  onEdit={() => setEditor({ mode: "edit", name: p.name })}
                  onDuplicate={() => onDuplicate(p.name)}
                  onRename={() => onRename(p.name)}
                  onDelete={() => onDelete(p.name)}
                />
              ))}
          </ul>
        </section>

        <footer>
          <button onClick={refresh}>Refresh</button>
          <button className="ghost" onClick={() => setShowHistory(true)}>
            History
          </button>
        </footer>
      </main>
    </>
  );
}

function ProfileRow(props: {
  name: string;
  isActive: boolean;
  reserved?: boolean;
  onToggle: () => void;
  onEdit: () => void;
  onDuplicate: () => void;
  onRename?: () => void;
  onDelete?: () => void;
}) {
  return (
    <li className={props.isActive ? "active" : ""}>
      <button className="row-main" onClick={props.onToggle}>
        <span className="dot" />
        <span className="name">{props.name}</span>
        {props.reserved && <em className="badge">backup</em>}
      </button>
      <div className="row-actions">
        <button className="icon" title="Edit" onClick={props.onEdit}>
          ✎
        </button>
        <button className="icon" title="Duplicate" onClick={props.onDuplicate}>
          ⎘
        </button>
        {props.onRename && (
          <button className="icon" title="Rename" onClick={props.onRename}>
            ⇄
          </button>
        )}
        {props.onDelete && (
          <button
            className="icon danger"
            title="Delete"
            onClick={props.onDelete}
          >
            🗑
          </button>
        )}
      </div>
    </li>
  );
}

export default App;
