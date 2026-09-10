import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";

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

type DriftInfo = {
  last_active: string;
  current_content: string;
  expected_content: string;
  unified_diff: string;
};

type SyncStatus = {
  linked: boolean;
  remote_url: string | null;
  branch: string | null;
  last_synced_sha: string | null;
  head_sha: string | null;
  auto_pull: boolean;
  auto_push: boolean;
};

type MaterializeOutcome = {
  kind: "created" | "unchanged" | "fast-forward" | "conflict" | "skipped";
  local?: string;
  remote?: string;
  reason?: string;
};

type MaterializeEntry = { name: string; outcome: MaterializeOutcome };

type PullReport = {
  entries: MaterializeEntry[];
  head_sha: string;
  advanced: boolean;
  conflicts: number;
};

type DoctreeInfo = {
  id: string;
  display: string;
  tags: string[];
  est_tokens: number;
  selected: boolean;
};

type DomainApply = { id: string; import_line: string; warnings: string[] };
type ApplyDoctreesResult = { applied: DomainApply[]; composed: boolean };

type Engine = "claude" | "codex";
type EngineScope = "claude" | "codex" | "both";
type LoadTiming = "always" | "on-demand" | "conditional";
type OriginClass = "company" | "personal" | "unknown" | "not-a-repo";

type ScanLayer = {
  path: string;
  layer: string;
  bytes: number;
  modified: string;
  token_estimate: number;
  engine: Engine;
  rank: number;
  kind: string;
  load: LoadTiming;
  origin_class: OriginClass;
  exists: boolean;
  in_prompt: boolean;
  imported_by: string | null;
};

type Capability = { engine: Engine; kind: string; name: string; source: string };

type Inventory = {
  cwd: string;
  scanned_at: string;
  layers: ScanLayer[];
  capabilities: Capability[];
};

type ContextReport = { report_id: string; report: string; prompt: string };

type Mode = "global" | "memory" | "sync" | "domains" | "context";

type EditorView =
  | { kind: "new" }
  | { kind: "edit"; name: string; readOnly?: boolean };

function App() {
  const [mode, setMode] = useState<Mode>("global");
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [active, setActive] = useState<string>("origin");
  const [drift, setDrift] = useState<DriftInfo | null>(null);
  const [showDrift, setShowDrift] = useState(false);

  const [memoryProjects, setMemoryProjects] = useState<MemoryProject[]>([]);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);

  const [editor, setEditor] = useState<EditorView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const hasDrift = drift !== null;

  const refreshGlobal = useCallback(async () => {
    try {
      const [list, current, driftInfo] = await Promise.all([
        invoke<ProfileSummary[]>("list_profiles"),
        invoke<string>("get_active_profile"),
        invoke<DriftInfo | null>("check_drift"),
      ]);
      setProfiles(list);
      setActive(current);
      setDrift(driftInfo);
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
    if (mode === "memory") {
      await refreshMemory();
    } else {
      // global / sync / domains all show the global active + drift state.
      await refreshGlobal();
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
      // Reuse the v0.1 change event plus the v0.3 sibling events so the popover
      // stays reactive after a background pull / doctree apply.
      const events = ["claude-md:changed", "repo:synced", "doctree:changed"];
      const unlisteners = await Promise.all(
        events.map((e) => listen(e, () => refresh())),
      );
      unlisten = () => unlisteners.forEach((u) => u());
    })();
    return () => {
      unlisten?.();
    };
  }, [refresh]);

  async function toggle(name: string) {
    if (busy) return;
    setError(null);
    // Drift present → surface the 4-button dialog instead of silently
    // discarding. The user resolves, then retries the toggle.
    if (mode === "global" && hasDrift) {
      setShowDrift(true);
      return;
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

  async function resolveDrift(
    cmd:
      | "resolve_drift_apply_to_active"
      | "resolve_drift_apply_to_origin"
      | "resolve_drift_discard",
  ) {
    try {
      await invoke(cmd);
      setShowDrift(false);
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

  const showProfileList = mode === "global" || mode === "memory";

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
              <button
                className="badge-edit"
                title="CLAUDE.md edited outside the app — click to resolve"
                onClick={() => setShowDrift(true)}
              >
                edited
              </button>
            )}
          </div>
          {showProfileList && (
            <button
              className="ico-btn"
              title="New profile"
              onClick={() => setEditor({ kind: "new" })}
            >
              +
            </button>
          )}
        </div>

        <div className="mode-switch" role="tablist">
          {(["global", "memory", "sync", "domains", "context"] as Mode[]).map((m) => (
            <button
              key={m}
              role="tab"
              aria-selected={mode === m}
              className={mode === m ? "on" : ""}
              onClick={() => {
                setMode(m);
                setEditor(null);
              }}
            >
              {m === "global"
                ? "Global"
                : m === "memory"
                  ? "Memory"
                  : m === "sync"
                    ? "Sync"
                    : m === "domains"
                      ? "Domains"
                      : "Context"}
            </button>
          ))}
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

      {showDrift && drift && (
        <DriftDialog
          drift={drift}
          onResolve={resolveDrift}
          onCancel={() => setShowDrift(false)}
        />
      )}

      {showProfileList && (
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
      )}

      {mode === "sync" && <SyncPanel onError={setError} onChanged={refresh} />}
      {mode === "domains" && <DomainsPanel onError={setError} />}
      {mode === "context" && <ContextPanel onError={setError} />}

      <footer className="pop-foot">
        <span className="caption">
          {mode === "memory"
            ? "~/.claude/projects/…/memory/MEMORY.md.*"
            : mode === "sync"
              ? "context repo ↔ ~/.claude/.toggler-sync"
              : mode === "domains"
                ? "~/.claude/domains/* (additive @import)"
                : mode === "context"
                  ? "read-only — nothing is written"
                  : "~/.claude/CLAUDE.md.*"}
        </span>
      </footer>
    </main>
  );
}

function DriftDialog(props: {
  drift: DriftInfo;
  onResolve: (
    cmd:
      | "resolve_drift_apply_to_active"
      | "resolve_drift_apply_to_origin"
      | "resolve_drift_discard",
  ) => void;
  onCancel: () => void;
}) {
  const { drift, onResolve, onCancel } = props;
  return (
    <div className="drift-modal" role="dialog" aria-label="Resolve external edit">
      <p className="drift-msg">
        <strong>CLAUDE.md was edited outside the app.</strong> Baseline:{" "}
        <code>{drift.last_active}</code>
      </p>
      <pre className="drift-diff">{drift.unified_diff}</pre>
      <div className="drift-actions">
        <button
          className="primary"
          title="Save the current edits back into this profile"
          onClick={() => onResolve("resolve_drift_apply_to_active")}
        >
          Keep edits → profile
        </button>
        <button
          title="Promote the current edits as the origin baseline"
          onClick={() => onResolve("resolve_drift_apply_to_origin")}
        >
          Keep edits → origin
        </button>
        <button
          className="danger"
          title="Throw the external edits away and restore the profile"
          onClick={() => onResolve("resolve_drift_discard")}
        >
          Discard edits
        </button>
        <button className="ghost" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}

function SyncPanel(props: {
  onError: (e: string | null) => void;
  onChanged: () => void;
}) {
  const { onError, onChanged } = props;
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [remoteUrl, setRemoteUrl] = useState("");
  const [branch, setBranch] = useState("main");
  const [pat, setPat] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [lastPull, setLastPull] = useState<PullReport | null>(null);

  const load = useCallback(async () => {
    try {
      setStatus(await invoke<SyncStatus>("get_sync_status"));
      onError(null);
    } catch (e) {
      onError(String(e));
    }
  }, [onError]);

  useEffect(() => {
    load();
  }, [load]);

  async function run(label: string, fn: () => Promise<void>) {
    setBusy(label);
    try {
      await fn();
      onError(null);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(null);
      await load();
    }
  }

  if (!status) return <div className="empty">Loading…</div>;

  if (!status.linked) {
    return (
      <div className="sync-panel">
        <p className="caption">Link a context repo to sync profiles + doc-trees.</p>
        <input
          className="name-input"
          placeholder="https://github.com/you/claude-md-context.git"
          value={remoteUrl}
          onChange={(e) => setRemoteUrl(e.target.value)}
        />
        <div className="sync-row">
          <input
            className="name-input small"
            placeholder="branch"
            value={branch}
            onChange={(e) => setBranch(e.target.value)}
          />
          <input
            className="name-input small"
            placeholder="PAT (optional)"
            type="password"
            value={pat}
            onChange={(e) => setPat(e.target.value)}
          />
        </div>
        <button
          className="primary"
          disabled={!remoteUrl || busy !== null}
          onClick={() =>
            run("link", async () => {
              await invoke("link_repo", {
                remoteUrl,
                branch,
                pat: pat || null,
              });
              onChanged();
            })
          }
        >
          {busy === "link" ? "Linking…" : "Link repo"}
        </button>
        <p className="caption tiny">
          Uses your OS credential helper (the token gh already stored) when no PAT
          is given.
        </p>
      </div>
    );
  }

  const synced = status.last_synced_sha?.slice(0, 7) ?? "—";
  const head = status.head_sha?.slice(0, 7) ?? "—";

  return (
    <div className="sync-panel">
      <div className="sync-status">
        <code className="repo-url">{status.remote_url}</code>
        <span className="caption tiny">
          branch <b>{status.branch}</b> · synced {synced} · head {head}
        </span>
      </div>

      <div className="sync-row">
        <button
          className="primary"
          disabled={busy !== null}
          onClick={() =>
            run("fetch", async () => {
              const report = await invoke<PullReport>("fetch_repo");
              setLastPull(report);
              onChanged();
            })
          }
        >
          {busy === "fetch" ? "Fetching…" : "Fetch ↓"}
        </button>
        <button
          disabled={busy !== null}
          onClick={() =>
            run("push", async () => {
              await invoke("push_repo");
            })
          }
        >
          {busy === "push" ? "Pushing…" : "Push ↑"}
        </button>
      </div>

      {lastPull && (
        <div className="pull-report">
          <span className="caption tiny">
            pulled {lastPull.head_sha.slice(0, 7)} ·{" "}
            {lastPull.conflicts > 0 ? (
              <b className="warn">{lastPull.conflicts} conflict(s) — resolve in Global</b>
            ) : (
              <span>{lastPull.entries.length} profile(s), no conflicts</span>
            )}
          </span>
        </div>
      )}

      <label className="sync-opt">
        <input
          type="checkbox"
          checked={status.auto_pull}
          onChange={(e) =>
            run("auto", () =>
              invoke("set_sync_auto", {
                autoPull: e.target.checked,
                autoPush: status.auto_push,
              }).then(() => {}),
            )
          }
        />
        auto-pull on startup
      </label>
      <label className="sync-opt">
        <input
          type="checkbox"
          checked={status.auto_push}
          onChange={(e) =>
            run("auto", () =>
              invoke("set_sync_auto", {
                autoPull: status.auto_pull,
                autoPush: e.target.checked,
              }).then(() => {}),
            )
          }
        />
        auto-push on change
      </label>

      <button
        className="ghost"
        disabled={busy !== null}
        onClick={() => run("unlink", () => invoke("unlink_repo").then(() => {}))}
      >
        Unlink
      </button>
    </div>
  );
}

// Mirror of the Rust validate_name + RESERVED_NAMES so the New doctree form
// rejects the same ids the backend would (avoids a confusing late server error).
const RESERVED_DOCTREE_IDS = new Set(["origin", "tmp", "composed"]);
function isValidDoctreeId(id: string): boolean {
  if (!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(id)) return false;
  if (RESERVED_DOCTREE_IDS.has(id) || id.startsWith("tmp.")) return false;
  return true;
}

type CreateDoctreeResult = {
  id: string;
  pushed: boolean;
  push_error: string | null;
};

function DomainsPanel(props: { onError: (e: string | null) => void }) {
  const { onError } = props;
  const [doctrees, setDoctrees] = useState<DoctreeInfo[] | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [applied, setApplied] = useState<ApplyDoctreesResult | null>(null);

  const [showNew, setShowNew] = useState(false);
  const [creating, setCreating] = useState(false);
  const [createMsg, setCreateMsg] = useState<string | null>(null);
  const [nid, setNid] = useState("");
  const [ndisplay, setNdisplay] = useState("");
  const [ntags, setNtags] = useState("");
  const [ntokens, setNtokens] = useState("");
  const [nbody, setNbody] = useState("");

  // Per-project binding (v0.4): when a project is picked, the panel scopes to
  // that project's selection and composes into its MEMORY.md instead of the
  // global CLAUDE.md.
  const [projects, setProjects] = useState<MemoryProject[]>([]);
  const [projectId, setProjectId] = useState<string | null>(null);

  useEffect(() => {
    invoke<MemoryProject[]>("memory_list_projects")
      .then(setProjects)
      .catch(() => {});
  }, []);

  const load = useCallback(async () => {
    try {
      const list = projectId
        ? await invoke<DoctreeInfo[]>("list_project_doctrees", { projectId })
        : await invoke<DoctreeInfo[]>("list_doctrees");
      setDoctrees(list);
      setSelected(new Set(list.filter((d) => d.selected).map((d) => d.id)));
      onError(null);
    } catch (e) {
      onError(String(e));
    }
  }, [onError, projectId]);

  useEffect(() => {
    load();
  }, [load]);

  function toggleId(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function apply() {
    setBusy(true);
    try {
      const ids = Array.from(selected);
      const result = projectId
        ? await invoke<ApplyDoctreesResult>("apply_project_doctrees", {
            projectId,
            ids,
          })
        : await invoke<ApplyDoctreesResult>("apply_doctrees", { ids });
      setApplied(result);
      onError(null);
      await load();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function bindDirectory() {
    const proj = projects.find((p) => p.id === projectId);
    if (!proj) return;
    try {
      await invoke("add_mapping", {
        dirPath: proj.label,
        target: `doctree:${proj.id}`,
        profileName: proj.id,
      });
      onError(null);
    } catch (e) {
      onError(String(e));
    }
  }

  async function create() {
    if (!isValidDoctreeId(nid)) {
      onError(
        "id must be lowercase a-z / 0-9 / hyphens, 1-64 chars, and not origin/tmp/composed.",
      );
      return;
    }
    setCreating(true);
    setCreateMsg(null);
    try {
      const res = await invoke<CreateDoctreeResult>("create_doctree", {
        id: nid,
        display: ndisplay,
        tags: ntags
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        estTokens: Number(ntokens) || 0,
        indexBody: nbody || null,
      });
      setCreateMsg(
        res.pushed
          ? `created & pushed '${res.id}'`
          : `created '${res.id}' locally — push rejected, pull then Push${res.push_error ? ` (${res.push_error})` : ""}`,
      );
      setNid("");
      setNdisplay("");
      setNtags("");
      setNtokens("");
      setNbody("");
      setShowNew(false);
      onError(null);
      await load();
    } catch (e) {
      onError(String(e));
    } finally {
      setCreating(false);
    }
  }

  if (!doctrees) return <div className="empty">Loading…</div>;

  const dirty =
    doctrees.some((d) => d.selected !== selected.has(d.id)) || applied === null;

  return (
    <div className="domains-panel">
      <select
        className="proj-pick"
        value={projectId ?? ""}
        onChange={(e) => setProjectId(e.target.value || null)}
      >
        <option value="">Global (~/.claude/CLAUDE.md)</option>
        {projects.map((p) => (
          <option key={p.id} value={p.id}>
            {p.label}
          </option>
        ))}
      </select>
      {projectId && (
        <div className="caption tiny">
          composes into this project's user-level MEMORY.md ·{" "}
          <button className="linklike" onClick={bindDirectory}>
            bind directory
          </button>
        </div>
      )}

      <div className="doctree-head">
        <button className="ghost" onClick={() => setShowNew((v) => !v)}>
          {showNew ? "Cancel" : "+ New doctree"}
        </button>
      </div>

      {showNew && (
        <div className="new-doctree">
          <input
            className="name-input small"
            placeholder="id (e.g. kubernetes)"
            value={nid}
            onChange={(e) => setNid(e.target.value)}
          />
          <input
            className="name-input small"
            placeholder="display name"
            value={ndisplay}
            onChange={(e) => setNdisplay(e.target.value)}
          />
          <div className="sync-row">
            <input
              className="name-input small"
              placeholder="tags (comma-sep)"
              value={ntags}
              onChange={(e) => setNtags(e.target.value)}
            />
            <input
              className="name-input small"
              placeholder="est tokens"
              value={ntokens}
              onChange={(e) => setNtokens(e.target.value)}
            />
          </div>
          <textarea
            className="content-area new-body"
            placeholder="# INDEX.md body (optional — a default skeleton is written if blank)"
            value={nbody}
            spellCheck={false}
            onChange={(e) => setNbody(e.target.value)}
          />
          <button
            className="primary"
            disabled={creating || !nid}
            onClick={create}
          >
            {creating ? "Creating…" : "Create & push"}
          </button>
        </div>
      )}
      {createMsg && <div className="caption tiny">{createMsg}</div>}

      {doctrees.length === 0 ? (
        <div className="empty">
          No doc-trees yet. Create one above, or link a repo with a{" "}
          <code>doctrees/</code> folder in Sync.
        </div>
      ) : (
        <>
          <ul className="doctree-list">
            {doctrees.map((d) => (
              <li key={d.id} className={selected.has(d.id) ? "sel" : ""}>
                <label className="doctree-row">
                  <input
                    type="checkbox"
                    checked={selected.has(d.id)}
                    onChange={() => toggleId(d.id)}
                  />
                  <span className="name">{d.display || d.id}</span>
                  {d.est_tokens > 0 && (
                    <span className="caption tiny">~{d.est_tokens} tok</span>
                  )}
                </label>
                {d.tags.length > 0 && (
                  <div className="tags">
                    {d.tags.map((t) => (
                      <span key={t} className="tag">
                        {t}
                      </span>
                    ))}
                  </div>
                )}
              </li>
            ))}
          </ul>

          <button className="primary" disabled={busy || !dirty} onClick={apply}>
            {busy
              ? "Applying…"
              : `Apply (${selected.size} domain${selected.size === 1 ? "" : "s"})`}
          </button>

          {applied && (
            <div className="caption tiny">
              {applied.composed
                ? `composed: base + ${applied.applied.length} domain(s) into ${projectId ? "this project's MEMORY.md" : "global CLAUDE.md"} — applies to new sessions`
                : "reverted to plain base profile"}
              {applied.applied.flatMap((a) => a.warnings).length > 0 && (
                <span className="warn">
                  {" "}
                  · {applied.applied.flatMap((a) => a.warnings).length} import
                  warning(s)
                </span>
              )}
            </div>
          )}
        </>
      )}
    </div>
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

  // Editor only operates on profile namespaces; sync/domains modes never open it.
  const memoryMode = mode === "memory";

  useEffect(() => {
    if (view.kind === "edit") {
      const cmd = memoryMode ? "memory_read_profile" : "read_profile";
      const args = memoryMode
        ? { projectId, name: view.name }
        : { name: view.name };
      invoke<string>(cmd, args)
        .then(setContent)
        .catch((e) => setError(String(e)));
    }
  }, [view, memoryMode, projectId]);

  async function onSave() {
    setError(null);
    if (isNew && !/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(name)) {
      setError("Name must be lowercase a-z, 0-9, hyphens; 1-64 chars.");
      return;
    }
    setSaving(true);
    try {
      if (!memoryMode) {
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
          {!memoryMode
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

/// Read-only inventory of what the agents actually load here (v0.5).
///
/// Nothing in this panel writes to disk. The one thing that leaves the
/// machine is what the user copies, which is why body inclusion is per-file
/// and files from a repository they classified as company carry a warning.
function ContextPanel(props: { onError: (e: string | null) => void }) {
  const { onError } = props;
  const [projects, setProjects] = useState<MemoryProject[]>([]);
  const [cwd, setCwd] = useState<string>("");
  const [scope, setScope] = useState<EngineScope>("both");
  const [inv, setInv] = useState<Inventory | null>(null);
  const [bodies, setBodies] = useState<Set<string>>(new Set());
  const [model, setModel] = useState("claude-opus-5");
  const [built, setBuilt] = useState<ContextReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  // A real repo contributes ~30 subtree files. Shown flat they bury the three
  // rows that actually cost tokens in every session.
  const [showRest, setShowRest] = useState<Set<Engine>>(new Set());

  useEffect(() => {
    invoke<MemoryProject[]>("memory_list_projects")
      .then((ps) => {
        setProjects(ps);
        if (!cwd && ps.length > 0) setCwd(ps[0].label);
      })
      .catch(() => {});
    // Only seeding the initial pick; re-running on cwd would fight the user.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const runScan = useCallback(async () => {
    if (!cwd) return;
    setBusy(true);
    setBuilt(null);
    try {
      const result = await invoke<Inventory>("scan_context", {
        cwd,
        engineScope: scope,
      });
      setInv(result);
      // A path that is no longer in the inventory must not stay opted in —
      // it would silently do nothing on the next build.
      setBodies((prev) => {
        const live = new Set(result.layers.map((l) => l.path));
        return new Set(Array.from(prev).filter((p) => live.has(p)));
      });
      onError(null);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }, [cwd, scope, onError]);

  useEffect(() => {
    runScan();
  }, [runScan]);

  async function build() {
    setBusy(true);
    try {
      const r = await invoke<ContextReport>("build_context_report", {
        cwd,
        options: {
          target_model: model,
          engine_scope: scope,
          include_bodies: Array.from(bodies),
        },
      });
      setBuilt(r);
      onError(null);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function copy(what: "report" | "prompt" | "both") {
    if (!built) return;
    const text =
      what === "report"
        ? built.report
        : what === "prompt"
          ? built.prompt
          : built.prompt;
    try {
      await writeText(text);
      const company = Array.from(bodies).filter((p) =>
        inv?.layers.some((l) => l.path === p && l.origin_class === "company"),
      ).length;
      setCopied(
        `copied ${text.length.toLocaleString()} chars` +
          (bodies.size > 0 ? ` · ${bodies.size} file bodies` : " · no bodies") +
          (company > 0 ? ` · ${company} from a company repo` : ""),
      );
      window.setTimeout(() => setCopied(null), 6000);
    } catch (e) {
      onError(String(e));
    }
  }

  function toggleBody(path: string) {
    setBodies((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  const engines: Engine[] =
    scope === "both" ? ["claude", "codex"] : [scope as Engine];

  function totals(engine: Engine) {
    const mine = (inv?.layers ?? []).filter(
      (l) => l.engine === engine && l.in_prompt,
    );
    const always = mine
      .filter((l) => l.load === "always")
      .reduce((a, l) => a + l.token_estimate, 0);
    const rest = mine
      .filter((l) => l.load !== "always")
      .reduce((a, l) => a + l.token_estimate, 0);
    return { always, rest };
  }

  return (
    <div className="ctx-panel">
      <div className="ctx-controls">
        <select value={cwd} onChange={(e) => setCwd(e.target.value)}>
          {projects.length === 0 && <option value="">— no projects —</option>}
          {projects.map((p) => (
            <option key={p.id} value={p.label}>
              {p.label}
            </option>
          ))}
        </select>
        <div className="ctx-scope">
          {(["both", "claude", "codex"] as EngineScope[]).map((s) => (
            <button
              key={s}
              className={scope === s ? "on" : ""}
              onClick={() => setScope(s)}
            >
              {s}
            </button>
          ))}
        </div>
      </div>

      {inv && (
        <>
          {engines.map((engine) => {
            const mine = inv.layers.filter((l) => l.engine === engine);
            if (mine.length === 0) return null;
            const t = totals(engine);
            const rest = mine.filter((l) => l.load !== "always");
            // Two different things hide in here: prompt text that loads on a
            // trigger, and config that never becomes prompt text at all.
            // Calling the second "loaded on trigger · ~0" would be wrong.
            const triggered = rest.filter((l) => l.in_prompt);
            const config = rest.filter((l) => !l.in_prompt);
            const restLabel = [
              triggered.length > 0
                ? `${triggered.length} on trigger · ~${t.rest.toLocaleString()}`
                : null,
              config.length > 0 ? `${config.length} config (not prompt text)` : null,
            ]
              .filter(Boolean)
              .join(" · ");
            const open = showRest.has(engine);
            const visible = open ? mine : mine.filter((l) => l.load === "always");
            return (
              <div key={engine} className="ctx-engine">
                <div className="ctx-engine-head">
                  <strong>{engine === "claude" ? "Claude Code" : "Codex CLI"}</strong>
                  <span className="caption">
                    ~{t.always.toLocaleString()} always · ~
                    {t.rest.toLocaleString()} on trigger
                  </span>
                </div>
                <ul className="ctx-layers">
                  {visible.map((l) => (
                    <li
                      key={`${l.engine}:${l.path}`}
                      className={l.load === "always" ? "always" : ""}
                    >
                      <label title={l.path}>
                        <input
                          type="checkbox"
                          checked={bodies.has(l.path)}
                          disabled={!l.in_prompt || !l.exists}
                          onChange={() => toggleBody(l.path)}
                        />
                        <span className="ctx-kind">{l.kind}</span>
                        <span className="ctx-path">{shortPath(l.path)}</span>
                      </label>
                      <span className="ctx-tok">
                        {l.in_prompt ? `~${l.token_estimate.toLocaleString()}` : "—"}
                        {l.origin_class === "company" && (
                          <span className="warn" title="from a company repository">
                            {" "}
                            ⚠
                          </span>
                        )}
                        {!l.exists && (
                          <span className="warn" title="import target is missing">
                            {" "}
                            ✕
                          </span>
                        )}
                      </span>
                    </li>
                  ))}
                </ul>
                {rest.length > 0 && (
                  <button
                    className="ctx-more"
                    onClick={() =>
                      setShowRest((prev) => {
                        const next = new Set(prev);
                        if (next.has(engine)) next.delete(engine);
                        else next.add(engine);
                        return next;
                      })
                    }
                  >
                    {open ? `hide ${rest.length} more` : `show ${restLabel}`}
                  </button>
                )}
              </div>
            );
          })}

          <div className="ctx-build">
            <input
              className="name-input"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="target model (who reads this harness)"
            />
            <button disabled={busy || !cwd} onClick={build}>
              {busy ? "…" : "Build report"}
            </button>
          </div>

          <p className="caption">
            {bodies.size === 0
              ? "Headings only — tick a file to include its text."
              : `${bodies.size} file ${bodies.size === 1 ? "body" : "bodies"} will be included.`}
          </p>
        </>
      )}

      {built && (
        <div className="ctx-out">
          <div className="ctx-out-head">
            <span className="caption">id {built.report_id.slice(0, 12)}</span>
            <div className="ctx-actions">
              <button onClick={() => copy("both")}>Copy prompt</button>
              <button className="linklike" onClick={() => copy("report")}>
                report only
              </button>
            </div>
          </div>
          {copied && <p className="caption">{copied}</p>}
          <textarea
            className="content-area ctx-preview"
            readOnly
            spellCheck={false}
            value={built.prompt}
          />
        </div>
      )}
    </div>
  );
}

/// Collapse the home prefix so a path fits the popover without hiding which
/// file it is.
function shortPath(p: string): string {
  const m = p.match(/^\/Users\/[^/]+\/(.*)$/);
  return m ? `~/${m[1]}` : p;
}
