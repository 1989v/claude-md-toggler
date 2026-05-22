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

function App() {
  const [mode, setMode] = useState<Mode>("global");
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [active, setActive] = useState<string>("origin");
  const [hasDrift, setHasDrift] = useState(false);

  const [memoryProjects, setMemoryProjects] = useState<MemoryProject[]>([]);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);

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

  // Load memory project list once so the mode switch is instant.
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

  // Realtime indicator when the underlying file changes outside the app.
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

    // Drift in global mode is a safety net — if the user hand-edited
    // CLAUDE.md since the last toggle, confirm discard before we overwrite
    // it. Memory toggles don't have an equivalent baseline yet.
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

  function activeLabel() {
    if (mode === "global") return active;
    if (!selectedProject) return "pick a project";
    return active;
  }

  return (
    <main className="pop">
      <header className="pop-head">
        <div className="title">
          <span className="dot-active" />
          <strong>{activeLabel()}</strong>
          {mode === "global" && hasDrift && (
            <span className="badge-edit" title="CLAUDE.md edited outside the app">
              edited
            </span>
          )}
        </div>
        <div className="mode-switch" role="tablist">
          <button
            role="tab"
            aria-selected={mode === "global"}
            className={mode === "global" ? "on" : ""}
            onClick={() => setMode("global")}
          >
            Global
          </button>
          <button
            role="tab"
            aria-selected={mode === "memory"}
            className={mode === "memory" ? "on" : ""}
            onClick={() => setMode("memory")}
          >
            Memory
          </button>
        </div>
      </header>

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
            className={
              (p.is_active ? "active " : "") + (busy === p.name ? "busy" : "")
            }
            onClick={() => toggle(p.name)}
            role="button"
          >
            <span className="dot" />
            <span className="name">{p.name}</span>
            {p.name === "origin" && <em className="hint">backup</em>}
          </li>
        ))}
      </ul>

      <footer className="pop-foot">
        <span className="caption">
          Edit <code>~/.claude/{mode === "global" ? "CLAUDE.md.*" : "projects/…/memory/MEMORY.md.*"}</code> in your editor.
        </span>
      </footer>
    </main>
  );
}

export default App;
