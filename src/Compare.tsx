import { useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Verdict = "remove" | "merge" | "move" | "rewrite" | "keep" | "other";

type AdoptTarget =
  | { kind: "global-profile" }
  | { kind: "memory-profile"; project_id: string }
  | { kind: "export-only" };

type Finding = {
  path: string;
  lines: [number, number] | null;
  verdict: Verdict;
  as_is: string;
  to_be: string;
  rationale: string;
  token_delta: number;
  target: AdoptTarget;
};

type Proposal = {
  schema_version: number | null;
  report_id: string | null;
  target_model: string | null;
  findings: Finding[];
  warnings: string[];
};

type AdoptResult = { profile: string; applied: number; refused: string[] };

/// Reads a pasted answer and shows what it proposes, side by side.
///
/// Acceptance is per finding, and where an accepted set lands is decided by
/// the backend from the path — the toggler writes profiles for files it owns
/// and exports everything else rather than editing a working tree it shares.
export default function Compare() {
  const [answer, setAnswer] = useState("");
  const [expectedId, setExpectedId] = useState("");
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [accepted, setAccepted] = useState<Set<number>>(new Set());
  const [profileName, setProfileName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<AdoptResult | null>(null);
  const [exported, setExported] = useState<string | null>(null);

  const stale =
    proposal?.report_id && expectedId && proposal.report_id !== expectedId;

  async function parse() {
    setBusy(true);
    setResult(null);
    try {
      const p = await invoke<Proposal>("parse_proposal", { answer });
      setProposal(p);
      setAccepted(new Set());
      setError(null);
    } catch (e) {
      setProposal(null);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const groups = useMemo(() => {
    const m = new Map<string, number[]>();
    (proposal?.findings ?? []).forEach((f, i) => {
      const list = m.get(f.path) ?? [];
      list.push(i);
      m.set(f.path, list);
    });
    return Array.from(m.entries());
  }, [proposal]);

  const acceptedList = Array.from(accepted);
  const acceptedFindings = acceptedList.map((i) => proposal!.findings[i]);
  const writable =
    acceptedFindings.length > 0 &&
    acceptedFindings.every((f) => f.target.kind !== "export-only") &&
    new Set(acceptedFindings.map((f) => JSON.stringify(f.target))).size === 1;
  const savedTokens = acceptedFindings.reduce((a, f) => a + f.token_delta, 0);

  function toggle(i: number) {
    setAccepted((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }

  function acceptAllIn(indices: number[]) {
    setAccepted((prev) => {
      const next = new Set(prev);
      const allOn = indices.every((i) => next.has(i));
      indices.forEach((i) => (allOn ? next.delete(i) : next.add(i)));
      return next;
    });
  }

  async function adopt() {
    setBusy(true);
    try {
      const r = await invoke<AdoptResult>("adopt_findings", {
        answer,
        accepted: acceptedList,
        profileName,
      });
      setResult(r);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function exportMd() {
    setBusy(true);
    try {
      const path = await invoke<string>("export_findings_to_file", {
        answer,
        accepted: acceptedList,
      });
      setExported(path);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="cmp">
      <header className="cmp-head">
        <h1>Harness findings</h1>
        <p className="caption">
          Paste the model's answer. Nothing is written until you accept
          findings and save them as a profile.
        </p>
      </header>

      {!proposal && (
        <section className="cmp-input">
          <textarea
            className="content-area"
            placeholder="Paste the full answer here — surrounding prose is fine."
            value={answer}
            spellCheck={false}
            onChange={(e) => setAnswer(e.target.value)}
          />
          <div className="cmp-input-foot">
            <input
              className="name-input"
              placeholder="report id from the report (optional — checks the answer matches)"
              value={expectedId}
              onChange={(e) => setExpectedId(e.target.value)}
            />
            <button disabled={busy || !answer.trim()} onClick={parse}>
              {busy ? "…" : "Read answer"}
            </button>
          </div>
        </section>
      )}

      {error && <p className="error">{error}</p>}

      {proposal && (
        <>
          <div className="cmp-bar">
            <span className="caption">
              {proposal.findings.length} findings
              {proposal.target_model ? ` · judged for ${proposal.target_model}` : ""}
              {proposal.report_id ? ` · ${proposal.report_id.slice(0, 12)}` : ""}
            </span>
            <button className="linklike" onClick={() => setProposal(null)}>
              paste another
            </button>
          </div>

          {stale && (
            <p className="warn">
              This answer is for a different snapshot than the id you entered.
              The files may have changed since — quoted text that no longer
              matches will be refused rather than applied.
            </p>
          )}

          {proposal.warnings.length > 0 && (
            <ul className="cmp-warnings">
              {proposal.warnings.map((w, i) => (
                <li key={i}>{w}</li>
              ))}
            </ul>
          )}

          {groups.map(([path, indices]) => {
            const target = proposal.findings[indices[0]].target;
            return (
              <section key={path} className="cmp-group">
                <div className="cmp-group-head">
                  <code>{path}</code>
                  <span className="caption">
                    {target.kind === "global-profile"
                      ? "can be saved as a profile"
                      : target.kind === "memory-profile"
                        ? `memory profile · ${target.project_id}`
                        : "export only — the toggler does not own this file"}
                  </span>
                  <button
                    className="linklike"
                    onClick={() => acceptAllIn(indices)}
                  >
                    toggle all
                  </button>
                </div>
                {indices.map((i) => {
                  const f = proposal.findings[i];
                  return (
                    <div key={i} className="cmp-finding">
                      <label className="cmp-finding-head">
                        <input
                          type="checkbox"
                          checked={accepted.has(i)}
                          onChange={() => toggle(i)}
                        />
                        <span className={`tag verdict-${f.verdict}`}>
                          {f.verdict}
                        </span>
                        {f.lines && (
                          <span className="caption">
                            lines {f.lines[0]}–{f.lines[1]}
                          </span>
                        )}
                        <span className="caption cmp-delta">
                          {f.token_delta > 0 ? "+" : ""}
                          {f.token_delta} tok
                        </span>
                        <span className="cmp-why">{f.rationale}</span>
                      </label>
                      {(f.as_is || f.to_be) && (
                        <div className="cmp-sides">
                          <pre className="cmp-as-is">{f.as_is || "—"}</pre>
                          <pre className="cmp-to-be">
                            {f.verdict === "remove" ? "(removed)" : f.to_be || "—"}
                          </pre>
                        </div>
                      )}
                    </div>
                  );
                })}
              </section>
            );
          })}

          <footer className="cmp-foot">
            <span className="caption">
              {accepted.size} accepted
              {accepted.size > 0
                ? savedTokens <= 0
                  ? ` · saves ~${Math.abs(savedTokens)} tokens`
                  : ` · adds ~${savedTokens} tokens`
                : ""}
            </span>
            <input
              className="name-input"
              placeholder="new profile name"
              value={profileName}
              onChange={(e) => setProfileName(e.target.value)}
            />
            <button
              disabled={busy || !writable || !profileName.trim()}
              onClick={adopt}
              title={
                writable
                  ? "save the accepted findings as a new profile"
                  : "accept findings from one owned file to save a profile"
              }
            >
              Save as profile
            </button>
            <button
              className="linklike"
              disabled={busy || accepted.size === 0}
              onClick={exportMd}
            >
              Export
            </button>
          </footer>

          {exported && (
            <p className="caption">
              Written to <code>{exported}</code>
            </p>
          )}

          {result && (
            <div className="cmp-result">
              <p>
                Saved <code>{result.profile}</code> with {result.applied}{" "}
                {result.applied === 1 ? "edit" : "edits"}.
              </p>
              {result.refused.length > 0 && (
                <>
                  <p className="warn">
                    {result.refused.length} not applied:
                  </p>
                  <ul>
                    {result.refused.map((r, i) => (
                      <li key={i}>{r}</li>
                    ))}
                  </ul>
                </>
              )}
            </div>
          )}
        </>
      )}
    </main>
  );
}
