import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import ProfileEditor, { type EditorTarget } from "./components/ProfileEditor";

export type ProfileSummary = {
  name: string;
  path: string;
  is_active: boolean;
};

function App() {
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [active, setActive] = useState<string>("origin");
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<EditorTarget | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await invoke<ProfileSummary[]>("list_profiles");
      const current = await invoke<string>("get_active_profile");
      setProfiles(list);
      setActive(current);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
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

  const onToggle = (name: string) =>
    safeInvoke(() => invoke("toggle_profile", { name }));

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

  return (
    <main className="app">
      <header>
        <h1>Claude.md Toggler</h1>
        <p className="subtitle">
          Active: <strong>{active}</strong>
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
            onEdit={() => setEditor({ mode: "edit", name: "origin", readOnly: true })}
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
      </footer>
    </main>
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
