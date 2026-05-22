import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type DirectoryMapping = {
  id: number;
  dir_path: string;
  target: string;
  profile_name: string;
  created_at: string;
  updated_at: string;
};

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

type ApplyResult = {
  matched: DirectoryMapping | null;
};

export default function MappingsView(props: { onClose: () => void }) {
  const { onClose } = props;
  const [mappings, setMappings] = useState<DirectoryMapping[]>([]);
  const [globalProfiles, setGlobalProfiles] = useState<string[]>([]);
  const [memoryProjects, setMemoryProjects] = useState<MemoryProject[]>([]);
  const [memoryProfilesByProject, setMemoryProfilesByProject] = useState<
    Record<string, string[]>
  >({});
  const [error, setError] = useState<string | null>(null);

  // Add-mapping form state.
  const [draftPath, setDraftPath] = useState("");
  const [draftTarget, setDraftTarget] = useState("global");
  const [draftProfile, setDraftProfile] = useState("");

  // Apply-for-path probe state.
  const [probePath, setProbePath] = useState("");
  const [probeStatus, setProbeStatus] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [list, profs, projs] = await Promise.all([
        invoke<DirectoryMapping[]>("list_mappings"),
        invoke<ProfileSummary[]>("list_profiles"),
        invoke<MemoryProject[]>("memory_list_projects"),
      ]);
      setMappings(list);
      setGlobalProfiles(["origin", ...profs.filter((p) => p.name !== "origin").map((p) => p.name)]);
      setMemoryProjects(projs);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Lazy-load memory profile names when a memory target is picked in the
  // add form. Cached by project id so re-selecting doesn't re-query.
  useEffect(() => {
    if (!draftTarget.startsWith("memory:")) return;
    const projectId = draftTarget.slice("memory:".length);
    if (memoryProfilesByProject[projectId]) return;
    invoke<ProfileSummary[]>("memory_list_profiles", { projectId })
      .then((profs) => {
        setMemoryProfilesByProject((prev) => ({
          ...prev,
          [projectId]: ["origin", ...profs.filter((p) => p.name !== "origin").map((p) => p.name)],
        }));
      })
      .catch((e) => setError(String(e)));
  }, [draftTarget, memoryProfilesByProject]);

  const availableProfiles = draftTarget === "global"
    ? globalProfiles
    : memoryProfilesByProject[draftTarget.slice("memory:".length)] ?? [];

  async function onAdd() {
    setError(null);
    if (!draftPath.trim()) {
      setError("Directory path is required.");
      return;
    }
    if (!draftProfile) {
      setError("Pick a profile to apply.");
      return;
    }
    try {
      await invoke("add_mapping", {
        dirPath: draftPath.trim(),
        target: draftTarget,
        profileName: draftProfile,
      });
      setDraftPath("");
      setDraftProfile("");
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function onDelete(id: number) {
    if (!confirm("Delete this mapping?")) return;
    try {
      await invoke("delete_mapping", { id });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  async function onApplyProbe() {
    setError(null);
    setProbeStatus(null);
    if (!probePath.trim()) {
      setError("Enter a directory path to apply mapping for.");
      return;
    }
    try {
      const result = await invoke<ApplyResult>("apply_mapping_for", {
        dirPath: probePath.trim(),
      });
      if (result.matched) {
        setProbeStatus(
          `Applied ${result.matched.target} / ${result.matched.profile_name} (rule: ${result.matched.dir_path})`,
        );
      } else {
        setProbeStatus("No rule matched this path.");
      }
    } catch (e) {
      setError(String(e));
    }
  }

  function targetLabel(target: string): string {
    if (target === "global") return "global";
    if (target.startsWith("memory:")) {
      const id = target.slice("memory:".length);
      const proj = memoryProjects.find((p) => p.id === id);
      return proj ? `memory · ${proj.label}` : target;
    }
    return target;
  }

  return (
    <main className="app">
      <header>
        <h1>Directory mappings</h1>
        <p className="subtitle">
          Apply a profile when a specific working directory is entered
        </p>
      </header>

      {error && <div className="error">{error}</div>}

      <section className="mappings-form">
        <h2>Add mapping</h2>
        <label className="field">
          <span>Directory path</span>
          <input
            type="text"
            value={draftPath}
            placeholder="/Users/me/work/project"
            onChange={(e) => setDraftPath(e.target.value)}
          />
        </label>
        <label className="field">
          <span>Target</span>
          <select
            value={draftTarget}
            onChange={(e) => {
              setDraftTarget(e.target.value);
              setDraftProfile("");
            }}
          >
            <option value="global">Global (~/.claude/CLAUDE.md)</option>
            {memoryProjects.map((p) => (
              <option key={p.id} value={`memory:${p.id}`}>
                Memory · {p.label}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Profile</span>
          <select
            value={draftProfile}
            onChange={(e) => setDraftProfile(e.target.value)}
            disabled={availableProfiles.length === 0}
          >
            <option value="">— pick a profile —</option>
            {availableProfiles.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
        </label>
        <div className="actions">
          <button className="primary" onClick={onAdd}>
            Add
          </button>
        </div>
      </section>

      <section className="profiles">
        <div className="section-head">
          <h2>Registered mappings</h2>
        </div>
        {mappings.length === 0 ? (
          <p className="hint">No mappings yet.</p>
        ) : (
          <ul>
            {mappings.map((m) => (
              <li key={m.id}>
                <div className="row-main" style={{ cursor: "default" }}>
                  <span className="name">
                    <code>{m.dir_path}</code>
                  </span>
                  <em className="badge">{targetLabel(m.target)}</em>
                  <span style={{ marginLeft: 8 }}>
                    → <code>{m.profile_name}</code>
                  </span>
                </div>
                <div className="row-actions">
                  <button
                    className="icon danger"
                    title="Delete"
                    onClick={() => onDelete(m.id)}
                  >
                    🗑
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="mappings-form">
        <h2>Apply mapping for…</h2>
        <p className="hint">
          Find and apply the rule matching a directory (longest-prefix wins).
        </p>
        <label className="field">
          <span>Directory path</span>
          <input
            type="text"
            value={probePath}
            placeholder="/Users/me/work/project/some/subdir"
            onChange={(e) => setProbePath(e.target.value)}
          />
        </label>
        <div className="actions">
          <button onClick={onApplyProbe}>Apply</button>
        </div>
        {probeStatus && <p className="hint">{probeStatus}</p>}
      </section>

      <footer>
        <button onClick={onClose}>Back</button>
      </footer>
    </main>
  );
}
