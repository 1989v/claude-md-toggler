import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { type EditorTarget } from "./ProfileEditor";

type MemoryProject = {
  id: string;
  label: string;
  memory_path: string;
  has_memory_file: boolean;
};

type ProfileSummary = {
  name: string;
  path: string;
  is_active: boolean;
};

const WARNING_KEY = "claude-md-toggler.memory-warning-ack";

export default function MemoryView(props: { onClose: () => void }) {
  const { onClose } = props;
  const [projects, setProjects] = useState<MemoryProject[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [active, setActive] = useState<string>("none");
  const [error, setError] = useState<string | null>(null);
  const [editor, setEditor] = useState<
    (EditorTarget & { projectId: string }) | null
  >(null);
  const [showWarning, setShowWarning] = useState(
    () => !localStorage.getItem(WARNING_KEY),
  );

  useEffect(() => {
    (async () => {
      try {
        const list = await invoke<MemoryProject[]>("memory_list_projects");
        setProjects(list);
        const firstWithMemory = list.find((p) => p.has_memory_file);
        if (firstWithMemory) {
          setSelected(firstWithMemory.id);
        }
      } catch (e) {
        setError(String(e));
      }
    })();
  }, []);

  const refresh = useCallback(async () => {
    if (!selected) {
      setProfiles([]);
      setActive("none");
      return;
    }
    try {
      const [list, current] = await Promise.all([
        invoke<ProfileSummary[]>("memory_list_profiles", {
          projectId: selected,
        }),
        invoke<string>("memory_get_active_profile", { projectId: selected }),
      ]);
      setProfiles(list);
      setActive(current);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, [selected]);

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

  const onToggle = (name: string) => {
    if (!selected) return;
    safeInvoke(() =>
      invoke("memory_toggle_profile", { projectId: selected, name }),
    );
  };

  const onDelete = (name: string) => {
    if (!selected) return;
    if (!confirm(`Delete memory profile "${name}"? This cannot be undone.`))
      return;
    safeInvoke(() =>
      invoke("memory_delete_profile", { projectId: selected, name }),
    );
  };

  const onRename = (name: string) => {
    if (!selected) return;
    const next = prompt(`Rename "${name}" to:`, name);
    if (!next || next === name) return;
    safeInvoke(() =>
      invoke("memory_rename_profile", {
        projectId: selected,
        oldName: name,
        newName: next,
      }),
    );
  };

  const onDuplicate = (name: string) => {
    if (!selected) return;
    const next = prompt(`Duplicate "${name}" as:`, `${name}-copy`);
    if (!next) return;
    safeInvoke(() =>
      invoke("memory_duplicate_profile", {
        projectId: selected,
        source: name,
        newName: next,
      }),
    );
  };

  function acknowledgeWarning() {
    localStorage.setItem(WARNING_KEY, new Date().toISOString());
    setShowWarning(false);
  }

  if (editor) {
    return (
      <MemoryEditorWrapper
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
    <>
      {showWarning && (
        <div className="overlay">
          <div className="dialog">
            <h2>About memory toggling</h2>
            <p className="hint">
              <strong>MEMORY.md</strong> accumulates context across your Claude
              Code sessions — preferences, project facts, lessons learned.
              Toggling a memory profile <strong>replaces</strong> the current
              contents until you switch back; if the file you toggle away from
              held learnings that no profile preserves, those learnings are
              lost (your <code>MEMORY.md.origin</code> backup still holds the
              starting state, but anything written between the last toggle and
              now does not).
            </p>
            <p className="hint">
              Recommended workflow: capture stable knowledge in a named
              profile (Duplicate &rarr; Edit) before toggling away.
            </p>
            <div className="dialog-actions">
              <button onClick={onClose}>Take me back</button>
              <button className="primary" onClick={acknowledgeWarning}>
                I understand — continue
              </button>
            </div>
          </div>
        </div>
      )}

      <main className="app">
        <header>
          <h1>Memory profiles</h1>
          <p className="subtitle">
            Per-project <code>MEMORY.md</code>
          </p>
        </header>

        {error && <div className="error">{error}</div>}

        <label className="field">
          <span>Project</span>
          <select
            value={selected ?? ""}
            onChange={(e) => setSelected(e.target.value || null)}
          >
            <option value="">— pick a project —</option>
            {projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.label}
                {p.has_memory_file ? "" : "  (no MEMORY.md yet)"}
              </option>
            ))}
          </select>
        </label>

        {selected && (
          <section className="profiles">
            <div className="section-head">
              <h2>Profiles</h2>
              <button
                className="ghost"
                onClick={() =>
                  setEditor({ mode: "new", projectId: selected })
                }
              >
                + New
              </button>
            </div>

            <ul>
              <MemoryRow
                name="origin"
                isActive={active === "origin"}
                onToggle={() => onToggle("origin")}
                onEdit={() =>
                  setEditor({
                    mode: "edit",
                    name: "origin",
                    readOnly: true,
                    projectId: selected,
                  })
                }
                onDuplicate={() => onDuplicate("origin")}
                reserved
              />
              {profiles
                .filter((p) => p.name !== "origin")
                .map((p) => (
                  <MemoryRow
                    key={p.name}
                    name={p.name}
                    isActive={p.is_active}
                    onToggle={() => onToggle(p.name)}
                    onEdit={() =>
                      setEditor({
                        mode: "edit",
                        name: p.name,
                        projectId: selected,
                      })
                    }
                    onDuplicate={() => onDuplicate(p.name)}
                    onRename={() => onRename(p.name)}
                    onDelete={() => onDelete(p.name)}
                  />
                ))}
            </ul>
            <p className="hint">
              Active: <strong>{active}</strong>
            </p>
          </section>
        )}

        <footer>
          <button onClick={onClose}>Back to global</button>
          <button className="ghost" onClick={refresh}>
            Refresh
          </button>
        </footer>
      </main>
    </>
  );
}

function MemoryRow(props: {
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

// ProfileEditor uses Tauri commands prefixed with "" (global flow). For memory
// we have to route to the memory_* equivalents — wrap the editor so callers
// don't need to know about the routing.
function MemoryEditorWrapper(props: {
  target: EditorTarget & { projectId: string };
  onClose: () => void;
  onSaved: () => void;
}) {
  const { target, onClose, onSaved } = props;
  const projectId = target.projectId;
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState(target.mode === "new" ? "" : target.name);
  const [content, setContent] = useState("");
  const [saving, setSaving] = useState(false);
  const readOnly = target.mode === "edit" && target.readOnly === true;
  const isNew = target.mode === "new";

  useEffect(() => {
    if (target.mode === "edit") {
      invoke<string>("memory_read_profile", {
        projectId,
        name: target.name,
      })
        .then(setContent)
        .catch((e) => setError(String(e)));
    }
  }, [projectId, target]);

  async function onSave() {
    setError(null);
    setSaving(true);
    try {
      if (isNew) {
        await invoke("memory_create_profile", { projectId, name, content });
      } else if (!readOnly) {
        await invoke("memory_update_profile", {
          projectId,
          name: target.name,
          content,
        });
      }
      onSaved();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  // Keep the visible look identical to the global ProfileEditor so users
  // don't develop two mental models.
  return (
    <main className="app editor">
      <header>
        <h1>
          {isNew
            ? "New memory profile"
            : readOnly
              ? `View "${target.name}"`
              : `Edit "${target.name}"`}
        </h1>
      </header>

      {error && <div className="error">{error}</div>}

      {isNew && (
        <label className="field">
          <span>Name</span>
          <input
            type="text"
            value={name}
            placeholder="e.g. project-context"
            onChange={(e) => setName(e.target.value)}
            autoFocus
          />
          <small>
            Stored as <code>MEMORY.md.{name || "{name}"}</code> next to the
            project's MEMORY.md
          </small>
        </label>
      )}

      <label className="field grow">
        <span>Content</span>
        <textarea
          value={content}
          onChange={(e) => setContent(e.target.value)}
          spellCheck={false}
          readOnly={readOnly}
        />
      </label>

      <footer className="actions">
        <button onClick={onClose} disabled={saving}>
          Cancel
        </button>
        {!readOnly && (
          <button className="primary" onClick={onSave} disabled={saving}>
            {saving ? "Saving…" : "Save"}
          </button>
        )}
      </footer>
    </main>
  );
}

