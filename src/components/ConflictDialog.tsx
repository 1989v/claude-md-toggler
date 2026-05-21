import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export type DriftInfo = {
  last_active: string;
  current_content: string;
  expected_content: string;
  unified_diff: string;
};

export type DriftChoice =
  | "apply-to-active"
  | "apply-to-origin"
  | "discard"
  | "cancel";

export default function ConflictDialog(props: {
  drift: DriftInfo;
  pendingProfile: string;
  onResolved: (choice: DriftChoice) => void;
}) {
  const { drift, pendingProfile, onResolved } = props;
  const [busy, setBusy] = useState<DriftChoice | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function choose(choice: DriftChoice) {
    setError(null);
    setBusy(choice);
    try {
      switch (choice) {
        case "apply-to-active":
          await invoke("resolve_drift_apply_to_active");
          break;
        case "apply-to-origin":
          await invoke("resolve_drift_apply_to_origin");
          break;
        case "discard":
          await invoke("resolve_drift_discard");
          break;
        case "cancel":
          break;
      }
      onResolved(choice);
    } catch (e) {
      setError(String(e));
      setBusy(null);
    }
  }

  return (
    <div className="overlay">
      <div className="dialog">
        <h2>Unsaved changes in CLAUDE.md</h2>
        <p className="hint">
          You've edited <code>CLAUDE.md</code> since the last toggle (
          <strong>{drift.last_active}</strong>). Choose how to handle these
          edits before switching to <strong>{pendingProfile}</strong>.
        </p>

        {error && <div className="error">{error}</div>}

        <pre className="diff">
          {drift.unified_diff || "(diff unavailable)"}
        </pre>

        <div className="dialog-actions">
          <button
            disabled={busy !== null}
            onClick={() => choose("apply-to-active")}
          >
            Save to <code>{drift.last_active}</code>
          </button>
          <button
            disabled={busy !== null}
            onClick={() => choose("apply-to-origin")}
          >
            Save to <code>origin</code>
          </button>
          <button
            className="danger"
            disabled={busy !== null}
            onClick={() => choose("discard")}
          >
            Discard edits
          </button>
          <button
            className="ghost"
            disabled={busy !== null}
            onClick={() => choose("cancel")}
          >
            Cancel toggle
          </button>
        </div>
      </div>
    </div>
  );
}
