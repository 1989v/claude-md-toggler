//! Reading a model's answer back in, and turning the parts the user accepts
//! into something the toggler can actually apply (v0.5).
//!
//! The answer arrives by paste, from whatever model the user asked. Three
//! decisions follow from that:
//!
//! - **Parse tolerantly, fail loudly per finding.** Models wrap JSON in prose,
//!   fence it, or drop a field. One bad finding must not discard the other
//!   twenty, so each is validated on its own and the rejects are reported by
//!   name instead of vanishing.
//!
//! - **Apply by content, never by line number.** A model's line numbers are a
//!   guess about a file it saw as text in a report; the file may also have
//!   changed since. Matching the quoted `as_is` means an edit either lands
//!   exactly where the model looked or does not land at all. An `as_is` that
//!   appears twice is ambiguous and is refused for the same reason — writing
//!   to the wrong one of two matches is worse than writing nothing.
//!
//! - **Only write what the toggler owns.** The global `CLAUDE.md` and a
//!   project's `MEMORY.md` have profile files, an origin backup and an atomic
//!   swap behind them. A `CLAUDE.md` inside the user's git tree has none of
//!   that and is shared with their editor and other sessions, so findings
//!   against it are exported, never applied.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::core::memory::MEMORY_TARGET_NAME;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("no JSON object found in the answer")]
    NoJson,
    #[error("the JSON is not an object")]
    NotAnObject,
    #[error("invalid JSON: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Remove,
    Merge,
    Move,
    Rewrite,
    Keep,
    /// Anything the answer used that the contract does not define. Kept rather
    /// than dropped so the user sees what the model actually said.
    Other,
}

impl Verdict {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "remove" => Verdict::Remove,
            "merge" => Verdict::Merge,
            "move" => Verdict::Move,
            "rewrite" => Verdict::Rewrite,
            "keep" => Verdict::Keep,
            _ => Verdict::Other,
        }
    }

    /// Whether acting on this verdict changes the file at all.
    pub fn is_edit(self) -> bool {
        matches!(
            self,
            Verdict::Remove | Verdict::Merge | Verdict::Move | Verdict::Rewrite
        )
    }
}

/// Where an accepted finding can land.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AdoptTarget {
    /// `~/.claude/CLAUDE.md` — has profiles, an origin backup and an atomic swap.
    GlobalProfile,
    /// `~/.claude/projects/{id}/memory/MEMORY.md` — same, per project.
    MemoryProfile { project_id: String },
    /// Anything else, including instruction files inside the user's git tree.
    /// Export only.
    ExportOnly,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub path: String,
    pub lines: Option<[u64; 2]>,
    pub verdict: Verdict,
    pub as_is: String,
    pub to_be: String,
    pub rationale: String,
    pub token_delta: i64,
    pub target: AdoptTarget,
}

#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub schema_version: Option<u64>,
    pub report_id: Option<String>,
    pub target_model: Option<String>,
    pub findings: Vec<Finding>,
    /// Everything that could not be read, named so the user can see what was
    /// dropped instead of silently getting fewer findings than the model gave.
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------

/// Pull the JSON object out of an answer that may be wrapped in prose.
///
/// Tries fenced blocks first (longest wins, since a model often shows a small
/// illustrative snippet alongside the real answer), then falls back to the
/// span between the first `{` and the last `}`.
pub fn extract_json(text: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let bytes: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < bytes.len() {
        let line = bytes[i].trim_start();
        if line.starts_with("```") || line.starts_with("~~~") {
            let marker = &line[..3];
            let mut j = i + 1;
            let mut body = Vec::new();
            while j < bytes.len() && !bytes[j].trim_start().starts_with(marker) {
                body.push(bytes[j]);
                j += 1;
            }
            let candidate = body.join("\n");
            if serde_json::from_str::<Value>(&candidate).is_ok()
                && best.as_ref().map_or(true, |b| candidate.len() > b.len())
            {
                best = Some(candidate);
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    if best.is_some() {
        return best;
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(text[start..=end].to_string())
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn as_string(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Read an answer into a [`Proposal`], classifying each finding's landing spot.
///
/// `claude_dir` decides ownership: paths under it may become profiles,
/// everything else is export-only.
pub fn parse_answer(text: &str, claude_dir: &Path) -> Result<Proposal, ParseError> {
    let raw = extract_json(text).ok_or(ParseError::NoJson)?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| ParseError::Invalid(e.to_string()))?;
    let obj = value.as_object().ok_or(ParseError::NotAnObject)?;

    let mut warnings = Vec::new();
    let mut findings = Vec::new();

    let items = match obj.get("findings") {
        Some(Value::Array(a)) => a.clone(),
        Some(_) => {
            warnings.push("`findings` was present but not an array".to_string());
            Vec::new()
        }
        None => {
            warnings.push("the answer has no `findings` array".to_string());
            Vec::new()
        }
    };

    for (i, item) in items.iter().enumerate() {
        let Some(f) = item.as_object() else {
            warnings.push(format!("finding {i}: not an object, skipped"));
            continue;
        };
        let Some(path) = as_string(f.get("path")).filter(|p| !p.trim().is_empty()) else {
            warnings.push(format!("finding {i}: no `path`, skipped"));
            continue;
        };
        let verdict = as_string(f.get("verdict"))
            .map(|s| Verdict::parse(&s))
            .unwrap_or(Verdict::Other);
        let as_is = as_string(f.get("as_is")).unwrap_or_default();
        if verdict.is_edit() && as_is.trim().is_empty() {
            warnings.push(format!(
                "finding {i} ({path}): {verdict:?} with no `as_is` to match, skipped"
            ));
            continue;
        }
        let lines = f.get("lines").and_then(|v| {
            let a = v.as_array()?;
            Some([a.first()?.as_u64()?, a.get(1)?.as_u64()?])
        });
        findings.push(Finding {
            target: classify_target(Path::new(&path), claude_dir),
            path,
            lines,
            verdict,
            as_is,
            to_be: as_string(f.get("to_be")).unwrap_or_default(),
            rationale: as_string(f.get("rationale")).unwrap_or_default(),
            token_delta: f.get("token_delta").and_then(|v| v.as_i64()).unwrap_or(0),
        });
    }

    Ok(Proposal {
        schema_version: obj.get("schema_version").and_then(|v| v.as_u64()),
        report_id: as_string(obj.get("report_id")),
        target_model: as_string(obj.get("target_model")),
        findings,
        warnings,
    })
}

/// Decide where a finding can land, from its path alone.
pub fn classify_target(path: &Path, claude_dir: &Path) -> AdoptTarget {
    if path == claude_dir.join("CLAUDE.md") {
        return AdoptTarget::GlobalProfile;
    }
    // {claude_dir}/projects/{id}/memory/MEMORY.md
    let projects = claude_dir.join("projects");
    if let Ok(rest) = path.strip_prefix(&projects) {
        let parts: Vec<_> = rest.components().collect();
        if parts.len() == 3
            && parts[1].as_os_str() == "memory"
            && parts[2].as_os_str() == MEMORY_TARGET_NAME
        {
            return AdoptTarget::MemoryProfile {
                project_id: parts[0].as_os_str().to_string_lossy().to_string(),
            };
        }
    }
    AdoptTarget::ExportOnly
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOutcome {
    pub applied: usize,
    /// Findings that were not applied, each with why. Never silent.
    pub refused: Vec<String>,
}

/// Apply the accepted findings to `original`, matching on the quoted `as_is`.
///
/// A finding is refused, not forced, when its `as_is` is absent from the file
/// or appears more than once. Both mean the model was looking at something
/// other than what is here now, and guessing at the right spot would corrupt
/// a file the user then toggles into place.
pub fn apply_findings(original: &str, findings: &[Finding]) -> (String, ApplyOutcome) {
    let mut out = original.to_string();
    let mut applied = 0;
    let mut refused = Vec::new();

    for f in findings {
        if !f.verdict.is_edit() {
            continue;
        }
        let hits = out.matches(f.as_is.as_str()).count();
        match hits {
            0 => refused.push(format!(
                "{}: the quoted text is not in the file (it may have changed since the report)",
                f.path
            )),
            1 => {
                out = out.replacen(f.as_is.as_str(), &f.to_be, 1);
                applied += 1;
            }
            n => refused.push(format!(
                "{}: the quoted text appears {n} times, so the right one is ambiguous",
                f.path
            )),
        }
    }
    (out, ApplyOutcome { applied, refused })
}

/// Render the findings the toggler will not write as a document the user can
/// take elsewhere.
pub fn export_markdown(proposal: &Proposal, findings: &[Finding]) -> String {
    let mut s = String::from("# Harness findings to apply by hand\n\n");
    if let Some(id) = &proposal.report_id {
        s.push_str(&format!("From report `{id}`"));
        if let Some(m) = &proposal.target_model {
            s.push_str(&format!(", judged for {m}"));
        }
        s.push_str(".\n\n");
    }
    s.push_str(
        "These live in files the toggler does not own — a working tree it shares with your \
editor and other sessions — so they are listed rather than written.\n",
    );
    let mut by_path: Vec<&Finding> = findings.iter().collect();
    by_path.sort_by(|a, b| a.path.cmp(&b.path));
    let mut current = "";
    for f in by_path {
        if f.path != current {
            s.push_str(&format!("\n## `{}`\n", f.path));
            current = &f.path;
        }
        let where_ = f
            .lines
            .map(|l| format!(" (lines {}–{})", l[0], l[1]))
            .unwrap_or_default();
        s.push_str(&format!(
            "\n### {:?}{}  ·  {:+} tokens\n\n{}\n\n",
            f.verdict, where_, f.token_delta, f.rationale
        ));
        s.push_str("as-is:\n\n```\n");
        s.push_str(&f.as_is);
        s.push_str("\n```\n");
        if !f.to_be.is_empty() {
            s.push_str("\nto-be:\n\n```\n");
            s.push_str(&f.to_be);
            s.push_str("\n```\n");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cdir() -> PathBuf {
        PathBuf::from("/home/u/.claude")
    }

    fn answer(findings: &str) -> String {
        format!(
            r#"{{"schema_version":1,"report_id":"abc123","target_model":"m","findings":[{findings}]}}"#
        )
    }

    fn one(path: &str, verdict: &str, as_is: &str, to_be: &str) -> String {
        format!(
            r#"{{"path":"{path}","lines":[1,2],"verdict":"{verdict}","as_is":"{as_is}","to_be":"{to_be}","rationale":"r","token_delta":-5}}"#
        )
    }

    // --- extraction ---

    #[test]
    fn json_is_found_inside_prose_and_a_fence() {
        let text = "Sure! Here is my review.\n\n```json\n{\"findings\":[]}\n```\n\nHope that helps.";
        let got = extract_json(text).unwrap();
        assert_eq!(got.trim(), "{\"findings\":[]}");
    }

    #[test]
    fn the_real_answer_wins_over_an_illustrative_snippet() {
        let text = "For example:\n```json\n{\"a\":1}\n```\nAnd the answer:\n```json\n{\"findings\":[],\"report_id\":\"xyz\",\"schema_version\":1}\n```";
        let got = extract_json(text).unwrap();
        assert!(got.contains("report_id"), "got {got}");
    }

    #[test]
    fn bare_json_with_no_fence_is_still_found() {
        let text = "Here you go: {\"findings\":[]} — done.";
        assert_eq!(extract_json(text).unwrap(), "{\"findings\":[]}");
    }

    #[test]
    fn an_answer_with_no_json_is_an_error() {
        assert!(matches!(
            parse_answer("I could not review this.", &cdir()),
            Err(ParseError::NoJson)
        ));
    }

    // --- tolerant parsing ---

    #[test]
    fn a_bad_finding_does_not_discard_the_good_ones() {
        let text = answer(&format!(
            "{},{},{}",
            one("/home/u/.claude/CLAUDE.md", "remove", "old rule", ""),
            r#"{"verdict":"remove","as_is":"x"}"#, // no path
            one("/home/u/.claude/CLAUDE.md", "rewrite", "a", "b"),
        ));
        let p = parse_answer(&text, &cdir()).unwrap();
        assert_eq!(p.findings.len(), 2);
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].contains("no `path`"));
    }

    #[test]
    fn an_edit_with_nothing_to_match_is_refused_at_parse_time() {
        let text = answer(&one("/home/u/.claude/CLAUDE.md", "rewrite", "", "new"));
        let p = parse_answer(&text, &cdir()).unwrap();
        assert!(p.findings.is_empty());
        assert!(p.warnings[0].contains("no `as_is`"), "{:?}", p.warnings);
    }

    #[test]
    fn keep_needs_no_as_is() {
        let text = answer(&one("/home/u/.claude/CLAUDE.md", "keep", "", ""));
        let p = parse_answer(&text, &cdir()).unwrap();
        assert_eq!(p.findings.len(), 1);
        assert_eq!(p.findings[0].verdict, Verdict::Keep);
    }

    #[test]
    fn an_unknown_verdict_is_surfaced_not_dropped() {
        let text = answer(&one("/home/u/.claude/CLAUDE.md", "obliterate", "x", ""));
        let p = parse_answer(&text, &cdir()).unwrap();
        assert_eq!(p.findings[0].verdict, Verdict::Other);
        assert!(!p.findings[0].verdict.is_edit(), "an unknown verdict must not edit");
    }

    #[test]
    fn the_snapshot_id_round_trips() {
        let p = parse_answer(&answer(""), &cdir()).unwrap();
        assert_eq!(p.report_id.as_deref(), Some("abc123"));
        assert_eq!(p.schema_version, Some(1));
    }

    // --- ownership ---

    #[test]
    fn the_global_harness_can_become_a_profile() {
        assert_eq!(
            classify_target(Path::new("/home/u/.claude/CLAUDE.md"), &cdir()),
            AdoptTarget::GlobalProfile
        );
    }

    #[test]
    fn a_project_memory_file_can_become_a_profile() {
        assert_eq!(
            classify_target(
                Path::new("/home/u/.claude/projects/-Users-x-p/memory/MEMORY.md"),
                &cdir()
            ),
            AdoptTarget::MemoryProfile {
                project_id: "-Users-x-p".to_string()
            }
        );
    }

    #[test]
    fn a_file_in_the_users_git_tree_is_export_only() {
        for p in [
            "/Users/x/IdeaProjects/msa/CLAUDE.md",
            "/Users/x/IdeaProjects/msa/AGENTS.md",
            "/home/u/.claude/settings.json",
            "/home/u/.claude/projects/-p/memory/other.md",
        ] {
            assert_eq!(
                classify_target(Path::new(p), &cdir()),
                AdoptTarget::ExportOnly,
                "{p} must not be writable by the toggler"
            );
        }
    }

    // --- applying ---

    #[test]
    fn a_rewrite_replaces_the_quoted_text() {
        let f = Finding {
            path: "p".into(),
            lines: None,
            verdict: Verdict::Rewrite,
            as_is: "be terse".into(),
            to_be: "be brief".into(),
            rationale: String::new(),
            token_delta: 0,
            target: AdoptTarget::GlobalProfile,
        };
        let (out, o) = apply_findings("# rules\nalways be terse here\n", &[f]);
        assert_eq!(out, "# rules\nalways be brief here\n");
        assert_eq!(o.applied, 1);
        assert!(o.refused.is_empty());
    }

    #[test]
    fn a_removal_deletes_the_quoted_text() {
        let f = Finding {
            path: "p".into(),
            lines: None,
            verdict: Verdict::Remove,
            as_is: "- redundant rule\n".into(),
            to_be: String::new(),
            rationale: String::new(),
            token_delta: -9,
            target: AdoptTarget::GlobalProfile,
        };
        let (out, o) = apply_findings("- keep\n- redundant rule\n- keep too\n", &[f]);
        assert_eq!(out, "- keep\n- keep too\n");
        assert_eq!(o.applied, 1);
    }

    #[test]
    fn text_that_is_not_in_the_file_is_refused_not_forced() {
        let f = Finding {
            path: "p".into(),
            lines: Some([3, 4]),
            verdict: Verdict::Rewrite,
            as_is: "a rule that was edited away".into(),
            to_be: "new".into(),
            rationale: String::new(),
            token_delta: 0,
            target: AdoptTarget::GlobalProfile,
        };
        let original = "# rules\nsomething else entirely\n";
        let (out, o) = apply_findings(original, &[f]);
        assert_eq!(out, original, "a miss must leave the file untouched");
        assert_eq!(o.applied, 0);
        assert_eq!(o.refused.len(), 1);
        assert!(o.refused[0].contains("not in the file"));
    }

    #[test]
    fn an_ambiguous_match_is_refused_rather_than_guessed() {
        let f = Finding {
            path: "p".into(),
            lines: None,
            verdict: Verdict::Rewrite,
            as_is: "- be brief".into(),
            to_be: "- be short".into(),
            rationale: String::new(),
            token_delta: 0,
            target: AdoptTarget::GlobalProfile,
        };
        let original = "## style\n- be brief\n## prose\n- be brief\n";
        let (out, o) = apply_findings(original, &[f]);
        assert_eq!(out, original, "writing to one of two matches would corrupt");
        assert_eq!(o.applied, 0);
        assert!(o.refused[0].contains("appears 2 times"));
    }

    #[test]
    fn keep_changes_nothing() {
        let f = Finding {
            path: "p".into(),
            lines: None,
            verdict: Verdict::Keep,
            as_is: "- be brief".into(),
            to_be: String::new(),
            rationale: String::new(),
            token_delta: 0,
            target: AdoptTarget::GlobalProfile,
        };
        let original = "- be brief\n";
        let (out, o) = apply_findings(original, &[f]);
        assert_eq!(out, original);
        assert_eq!(o.applied, 0);
        assert!(o.refused.is_empty(), "a keep is not a refusal");
    }

    #[test]
    fn several_edits_apply_in_order() {
        let mk = |a: &str, b: &str| Finding {
            path: "p".into(),
            lines: None,
            verdict: Verdict::Rewrite,
            as_is: a.into(),
            to_be: b.into(),
            rationale: String::new(),
            token_delta: 0,
            target: AdoptTarget::GlobalProfile,
        };
        let (out, o) = apply_findings("one two three\n", &[mk("one", "1"), mk("three", "3")]);
        assert_eq!(out, "1 two 3\n");
        assert_eq!(o.applied, 2);
    }

    // --- export ---

    #[test]
    fn export_lists_both_sides_and_groups_by_file() {
        let p = parse_answer(
            &answer(&format!(
                "{},{}",
                one("/repo/CLAUDE.md", "remove", "gone", ""),
                one("/repo/AGENTS.md", "rewrite", "old", "new"),
            )),
            &cdir(),
        )
        .unwrap();
        let md = export_markdown(&p, &p.findings);
        assert!(md.contains("## `/repo/AGENTS.md`"));
        assert!(md.contains("## `/repo/CLAUDE.md`"));
        assert!(md.contains("as-is:"));
        assert!(md.contains("to-be:"));
        assert!(md.contains("abc123"), "the snapshot must be traceable");
    }

    #[test]
    fn export_omits_the_to_be_block_for_a_removal() {
        let p = parse_answer(
            &answer(&one("/repo/CLAUDE.md", "remove", "gone", "")),
            &cdir(),
        )
        .unwrap();
        let md = export_markdown(&p, &p.findings);
        assert!(md.contains("as-is:"));
        assert!(!md.contains("to-be:"), "a removal has no replacement text");
    }
}
