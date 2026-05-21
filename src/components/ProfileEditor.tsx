import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type EditorTarget =
  | { mode: "new" }
  | { mode: "edit"; name: string; readOnly?: boolean };

const NAME_RX = /^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/;

function validateName(value: string): string | null {
  if (!value) return "Name is required.";
  if (value === "origin" || value === "tmp" || value.startsWith("tmp."))
    return "Name is reserved.";
  if (!NAME_RX.test(value))
    return "Use lowercase letters, digits and hyphens only (no leading/trailing hyphen).";
  return null;
}

export default function ProfileEditor(props: {
  target: EditorTarget;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { target, onClose, onSaved } = props;
  const isNew = target.mode === "new";
  const readOnly = target.mode === "edit" && target.readOnly === true;

  const [name, setName] = useState(isNew ? "" : target.name);
  const [content, setContent] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (target.mode === "edit") {
      invoke<string>("read_profile", { name: target.name })
        .then(setContent)
        .catch((e) => setError(String(e)));
    }
  }, [target]);

  async function onSave() {
    setError(null);
    const nameErr = isNew ? validateName(name) : null;
    if (nameErr) {
      setError(nameErr);
      return;
    }
    setSaving(true);
    try {
      if (isNew) {
        await invoke("create_profile", { name, content });
      } else if (!readOnly) {
        await invoke("update_profile", { name: target.name, content });
      }
      onSaved();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <main className="app editor">
      <header>
        <h1>
          {isNew ? "New profile" : readOnly ? `View "${target.name}"` : `Edit "${target.name}"`}
        </h1>
      </header>

      {error && <div className="error">{error}</div>}

      {isNew && (
        <label className="field">
          <span>Name</span>
          <input
            type="text"
            value={name}
            placeholder="e.g. debug-mode"
            onChange={(e) => setName(e.target.value)}
            autoFocus
          />
          <small>Stored as <code>~/.claude/CLAUDE.md.{name || "{name}"}</code></small>
        </label>
      )}

      <label className="field grow">
        <span>Content</span>
        <textarea
          value={content}
          onChange={(e) => setContent(e.target.value)}
          spellCheck={false}
          placeholder="# Profile harness content..."
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
