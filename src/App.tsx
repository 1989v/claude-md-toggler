import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type ProfileSummary = {
  name: string;
  path: string;
  is_active: boolean;
};

type MemoryProject = {
  id: string;
  label: string;
  memory_path: string;
  has_memory_file: boolean;
};

type Mode = "global" | "memory";

type EditorView =
  | { kind: "new" }
  | { kind: "edit"; name: string; readOnly?: boolean };

function App() {
  const [mode, setMode] = useState<Mode>("global");
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [active, setActive] = useState<string>("origin");
  const [hasDrift, setHasDrift] = useState(false);

  const [memoryProjects, setMemoryProjects] = useState<MemoryProject[]>([]);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);

  const [editor, setEditor] = useState<EditorView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const refreshGlobal = useCallback(async () => {
    try {
      const [list, current, drift] = await Promise.all([
        invoke<ProfileSummary[]>("list_profiles"),
        invoke<string>("get_active_profile"),
        invoke<unknown>("check_drift"),
      ]);
      setProfiles(list);
      setActive(current);
      setHasDrift(drift !== null);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const refreshMemory = useCallback(async () => {
    if (!selectedProject) {
      setProfiles([]);
      setActive("none");
      return;
    }
    try {
      const [list, current] = await Promise.all([
        invoke<ProfileSummary[]>("memory_list_profiles", {
          projectId: selectedProject,
        }),
        invoke<string>("memory_get_active_profile", {
          projectId: selectedProject,
        }),
      ]);
      setProfiles(list);
      setActive(current);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, [selectedProject]);

  const refresh = useCallback(async () => {
    if (mode === "global") {
      await refreshGlobal();
    } else {
      await refreshMemory();
    }
  }, [mode, refreshGlobal, refreshMemory]);

  useEffect(() => {
    invoke<MemoryProject[]>("memory_list_projects")
      .then((list) => {
        setMemoryProjects(list);
        const firstWithFile = list.find((p) => p.has_memory_file);
        if (firstWithFile) setSelectedProject(firstWithFile.id);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

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

  async function toggle(name: string) {
    if (busy) return;
    setError(null);

    if (mode === "global" && hasDrift) {
      const ok = confirm(
        "CLAUDE.md was edited outside the app. Discard those edits and switch profiles?",
      );
      if (!ok) return;
      try {
        await invoke("resolve_drift_discard");
      } catch (e) {
        setError(String(e));
        return;
      }
    }

    setBusy(name);
    try {
      if (mode === "global") {
        await invoke("toggle_profile", { name });
      } else {
        if (!selectedProject) return;
        await invoke("memory_toggle_profile", {
          projectId: selectedProject,
          name,
        });
      }
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function onDelete(name: string) {
    if (!confirm(`Delete profile "${name}"?`)) return;
    try {
      if (mode === "global") {
        await invoke("delete_profile", { name });
      } else if (selectedProject) {
        await invoke("memory_delete_profile", {
          projectId: selectedProject,
          name,
        });
      }
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  if (editor) {
    return (
      <Editor
        view={editor}
        mode={mode}
        projectId={selectedProject}
        onClose={() => setEditor(null)}
        onSaved={async () => {
          setEditor(null);
          await refresh();
        }}
      />
    );
  }

  return (
    <main className="pop">
      <header className="pop-head">
        <div className="title-row">
          <div className="title">
            <span className="dot-active" />
            <span className="active-name">
              {mode === "memory" && !selectedProject ? "pick a project" : active}
            </span>
            {mode === "global" && hasDrift && (
              <span className="badge-edit" title="CLAUDE.md edited outside the app">
                edited
              </span>
            )}
          </div>
          <button
            className="ico-btn"
            title="New profile"
            onClick={() => setEditor({ kind: "new" })}
          >
            +
          </button>
        </div>

        <div className="mode-switch" role="tablist">
          <button
            role="tab"
            aria-selected={mode === "global"}
            className={mode === "global" ? "on" : ""}
            onClick={() => {
              setMode("global");
              setEditor(null);
            }}
          >
            Global
          </button>
          <button
            role="tab"
            aria-selected={mode === "memory"}
            className={mode === "memory" ? "on" : ""}
            onClick={() => {
              setMode("memory");
              setEditor(null);
            }}
          >
            Memory
          </button>
        </div>

        {mode === "memory" && (
          <select
            className="proj-pick"
            value={selectedProject ?? ""}
            onChange={(e) => setSelectedProject(e.target.value || null)}
          >
            <option value="">— pick a project —</option>
            {memoryProjects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label}
                {p.has_memory_file ? "" : " (no MEMORY.md)"}
              </option>
            ))}
          </select>
        )}
      </header>

      {error && <div className="error compact">{error}</div>}

      <ul className="prof-list">
        {profiles.length === 0 && mode === "memory" && !selectedProject && (
          <li className="empty">Pick a project above.</li>
        )}
        {profiles.length === 0 && (mode === "global" || selectedProject) && (
          <li className="empty">No profile files found.</li>
        )}
        {profiles.map((p) => (
          <li
            key={p.name}
            className={[
              p.is_active ? "active" : "",
              busy === p.name ? "busy" : "",
            ]
              .filter(Boolean)
              .join(" ")}
          >
            <button
              className="row-main"
              onClick={() => toggle(p.name)}
              title={`Toggle to ${p.name}`}
            >
              <span className="dot" />
              <span className="name">{p.name}</span>
              {p.name === "origin" && <em className="hint">backup</em>}
            </button>
            <div className="row-actions">
              <button
                className="ico-btn"
                title={p.name === "origin" ? "View" : "Edit"}
                onClick={() =>
                  setEditor({
                    kind: "edit",
                    name: p.name,
                    readOnly: p.name === "origin",
                  })
                }
              >
                ✎
              </button>
              {p.name !== "origin" && (
                <button
                  className="ico-btn danger"
                  title="Delete"
                  onClick={() => onDelete(p.name)}
                >
                  ⊖
                </button>
              )}
            </div>
          </li>
        ))}
      </ul>

      <footer className="pop-foot">
        <span className="caption">
          {mode === "global"
            ? "~/.claude/CLAUDE.md.*"
            : "~/.claude/projects/…/memory/MEMORY.md.*"}
        </span>
      </footer>
    </main>
  );
}

function Editor(props: {
  view: EditorView;
  mode: Mode;
  projectId: string | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { view, mode, projectId, onClose, onSaved } = props;
  const isNew = view.kind === "new";
  const readOnly = view.kind === "edit" && view.readOnly === true;

  const [name, setName] = useState(isNew ? "" : view.name);
  const [content, setContent] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (view.kind === "edit") {
      const cmd = mode === "global" ? "read_profile" : "memory_read_profile";
      const args =
        mode === "global"
          ? { name: view.name }
          : { projectId, name: view.name };
      invoke<string>(cmd, args)
        .then(setContent)
        .catch((e) => setError(String(e)));
    }
  }, [view, mode, projectId]);

  async function onSave() {
    setError(null);
    if (isNew && !/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(name)) {
      setError("Name must be lowercase a-z, 0-9, hyphens; 1-64 chars.");
      return;
    }
    setSaving(true);
    try {
      if (mode === "global") {
        if (isNew) {
          await invoke("create_profile", { name, content });
        } else if (!readOnly) {
          await invoke("update_profile", { name: view.name, content });
        }
      } else {
        if (!projectId) {
          setError("No project selected.");
          setSaving(false);
          return;
        }
        if (isNew) {
          await invoke("memory_create_profile", { projectId, name, content });
        } else if (!readOnly) {
          await invoke("memory_update_profile", {
            projectId,
            name: view.name,
            content,
          });
        }
      }
      onSaved();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  const heading = isNew
    ? "New profile"
    : readOnly
      ? `View ${view.name}`
      : `Edit ${view.name}`;

  return (
    <main className="pop editor-mode">
      <header className="ed-head">
        <button className="back-btn" onClick={onClose} title="Back">
          ‹
        </button>
        <h2>{heading}</h2>
      </header>

      {error && <div className="error compact">{error}</div>}

      {isNew && (
        <input
          className="name-input"
          type="text"
          value={name}
          placeholder="profile-name"
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      )}

      <textarea
        className="content-area"
        value={content}
        onChange={(e) => setContent(e.target.value)}
        spellCheck={false}
        readOnly={readOnly}
        placeholder="# Harness content…"
      />

      <footer className="ed-foot">
        <span className="caption">
          {mode === "global"
            ? `~/.claude/CLAUDE.md.${isNew ? name || "{name}" : view.kind === "edit" ? view.name : ""}`
            : `MEMORY.md.${isNew ? name || "{name}" : view.kind === "edit" ? view.name : ""}`}
        </span>
        <div className="actions">
          <button onClick={onClose} disabled={saving} className="ghost">
            Cancel
          </button>
          {!readOnly && (
            <button onClick={onSave} disabled={saving} className="primary">
              {saving ? "Saving…" : "Save"}
            </button>
          )}
        </div>
      </footer>
    </main>
  );
}

export default App;
