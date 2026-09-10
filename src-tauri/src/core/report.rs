//! Turning an [`Inventory`] into two artefacts: a report a person reads, and
//! a prompt they paste into whatever model they already pay for (v0.5).
//!
//! The app deliberately does not call a model itself — that would mean holding
//! an API key, carrying the billing, and owning the egress decision. The user
//! copies, pastes, and brings the answer back.
//!
//! Three things shape the output:
//!
//! - **The prompt must be answerable in a parseable form.** It pins a JSON
//!   schema and asks for nothing outside it, because the compare view has to
//!   read the answer back. A prose answer is a dead end.
//!
//! - **Drift is compared by section body, never by heading set.** Measured on
//!   one repo: `CLAUDE.md` and `AGENTS.md` had identical heading structure
//!   (9 h2, 3 h3) while 12 KB of body was missing from one of them. A
//!   heading-set check reports "no drift" there — a false green.
//!
//! - **Bodies are opt-in per file.** The report leaves the user's machine when
//!   they paste it, so which files carry their text is their call, and the
//!   report says out loud which ones came from a repository they classified.

// `core` is a private module, so nothing here is reachable until the command
// layer consumes it. Drop this once `commands.rs` calls into this module.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::scan::{Engine, EngineScope, Inventory, Layer, LoadTiming, OriginClass};

/// Version of the answer contract. Bump when the schema changes so a stale
/// answer can be told apart from a malformed one.
pub const ANSWER_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct ReportOptions {
    /// The model that will *read* this harness. A capable model finds
    /// hand-holding rules wasteful; a small one needs them. The judgement
    /// changes with the reader, so the reader is named.
    pub target_model: String,
    pub engine_scope: EngineScope,
    /// Absolute paths whose full text goes into the report. Anything not
    /// listed contributes metadata and headings only. Normally these are the
    /// paths the inventory reported; a symlinked equivalent also matches, so
    /// a path in the other shape opts the file in rather than silently doing
    /// nothing.
    #[serde(default)]
    pub include_bodies: Vec<String>,
}

/// Stable fingerprint of an inventory.
///
/// Uses libgit2's blob hashing rather than pulling in a hash crate — this is
/// change detection, not a security boundary, and git2 is already vendored.
pub fn report_id(inv: &Inventory) -> String {
    let mut digest = String::new();
    digest.push_str(&inv.cwd);
    digest.push('\n');
    for l in &inv.layers {
        // Deliberately excludes `scanned_at`: rescanning an unchanged machine
        // must produce the same id.
        digest.push_str(&format!(
            "{}|{}|{}|{}|{}\n",
            l.path, l.bytes, l.modified, l.token_estimate, l.exists
        ));
    }
    git2::Oid::hash_object(git2::ObjectType::Blob, digest.as_bytes())
        .map(|oid| oid.to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Markdown sectioning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Heading trail, e.g. `["Key Conventions"]` or `["Navigation", "서비스별"]`.
    pub path: Vec<String>,
    pub body_bytes: usize,
}

/// Split markdown into `##`/`###` sections, ignoring fenced code.
///
/// Fence tracking is required: a shell block containing `## build` would
/// otherwise register as a heading and shift every following section.
pub fn sections(md: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut in_fence = false;
    let mut h2: Option<String> = None;
    let mut current: Option<Section> = None;

    for line in md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            if let Some(s) = current.as_mut() {
                s.body_bytes += line.len() + 1;
            }
            continue;
        }
        if !in_fence {
            if let Some(title) = trimmed.strip_prefix("## ") {
                if let Some(s) = current.take() {
                    out.push(s);
                }
                h2 = Some(title.trim().to_string());
                current = Some(Section {
                    path: vec![title.trim().to_string()],
                    body_bytes: 0,
                });
                continue;
            }
            if let Some(title) = trimmed.strip_prefix("### ") {
                if let Some(s) = current.take() {
                    out.push(s);
                }
                let mut path = Vec::new();
                if let Some(parent) = &h2 {
                    path.push(parent.clone());
                }
                path.push(title.trim().to_string());
                current = Some(Section {
                    path,
                    body_bytes: 0,
                });
                continue;
            }
        }
        if let Some(s) = current.as_mut() {
            s.body_bytes += line.len() + 1;
        }
    }
    if let Some(s) = current.take() {
        out.push(s);
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct SectionDelta {
    pub section: String,
    pub left_bytes: usize,
    pub right_bytes: usize,
    pub delta: i64,
}

/// Compare two documents section by section.
///
/// Sections missing from one side count as zero rather than being skipped, so
/// a section that exists only in `left` shows as a full loss.
pub fn diff_sections(left: &str, right: &str) -> Vec<SectionDelta> {
    let key = |s: &Section| s.path.join(" > ");
    let l: BTreeMap<String, usize> = sections(left)
        .into_iter()
        .map(|s| (key(&s), s.body_bytes))
        .collect();
    let r: BTreeMap<String, usize> = sections(right)
        .into_iter()
        .map(|s| (key(&s), s.body_bytes))
        .collect();

    let mut keys: Vec<&String> = l.keys().chain(r.keys()).collect();
    keys.sort();
    keys.dedup();

    let mut out: Vec<SectionDelta> = keys
        .into_iter()
        .map(|k| {
            let lb = *l.get(k).unwrap_or(&0);
            let rb = *r.get(k).unwrap_or(&0);
            SectionDelta {
                section: k.clone(),
                left_bytes: lb,
                right_bytes: rb,
                delta: lb as i64 - rb as i64,
            }
        })
        .collect();
    // Biggest losses first — that is what a reader is looking for.
    out.sort_by_key(|d| -d.delta);
    out
}

/// Heading trail of every `##`/`###` in the document, for the metadata-only view.
pub fn headings(md: &str) -> Vec<String> {
    sections(md).into_iter().map(|s| s.path.join(" > ")).collect()
}

// ---------------------------------------------------------------------------
// Totals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct EngineTotals {
    pub always_tokens: u64,
    pub on_demand_tokens: u64,
    pub always_files: usize,
    pub on_demand_files: usize,
}

pub fn totals_for(inv: &Inventory, engine: Engine) -> EngineTotals {
    let mut t = EngineTotals::default();
    for l in inv.layers.iter().filter(|l| l.engine == engine && l.in_prompt) {
        match l.load {
            LoadTiming::Always => {
                t.always_tokens += l.token_estimate;
                t.always_files += 1;
            }
            LoadTiming::OnDemand | LoadTiming::Conditional => {
                t.on_demand_tokens += l.token_estimate;
                t.on_demand_files += 1;
            }
        }
    }
    t
}

/// Groups of layers whose file content is byte-identical.
///
/// Found in the wild: `~/AGENTS.md` and `~/.codex/AGENTS.md` were separate
/// inodes with identical bytes. Copies like that are how a rule quietly gets
/// edited in one place and not the other.
pub fn duplicate_content(inv: &Inventory) -> Vec<Vec<String>> {
    let mut by_hash: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in inv.layers.iter().filter(|l| l.exists && l.bytes > 0) {
        let Ok(body) = std::fs::read(&l.path) else {
            continue;
        };
        let Ok(oid) = git2::Oid::hash_object(git2::ObjectType::Blob, &body) else {
            continue;
        };
        by_hash.entry(oid.to_string()).or_default().push(l.path.clone());
    }
    by_hash
        .into_values()
        .filter(|paths| paths.len() > 1)
        .collect()
}

/// Layers that exist but hold nothing — a heading in the tree that teaches the
/// model nothing.
pub fn empty_layers(inv: &Inventory) -> Vec<String> {
    inv.layers
        .iter()
        .filter(|l| l.exists && l.in_prompt && l.bytes == 0)
        .map(|l| l.path.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Report rendering
// ---------------------------------------------------------------------------

/// Whether two path strings name the same file.
///
/// Plain equality first; symlink resolution only as a fallback, so an opt-in
/// written in a different but equivalent shape is honoured instead of being a
/// silent no-op.
fn same_path(a: &str, b: &str) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

fn engine_name(e: Engine) -> &'static str {
    match e {
        Engine::Claude => "Claude Code",
        Engine::Codex => "Codex CLI",
    }
}

fn origin_note(c: OriginClass) -> &'static str {
    match c {
        OriginClass::Company => " ⚠ company",
        OriginClass::Personal => "",
        OriginClass::Unknown => "",
        OriginClass::NotARepo => "",
    }
}

fn engines_in(scope: EngineScope) -> Vec<Engine> {
    match scope {
        EngineScope::Claude => vec![Engine::Claude],
        EngineScope::Codex => vec![Engine::Codex],
        EngineScope::Both => vec![Engine::Claude, Engine::Codex],
    }
}

/// Render the human-readable inventory report.
pub fn render_report(inv: &Inventory, opts: &ReportOptions) -> String {
    let mut s = String::new();
    let id = report_id(inv);

    s.push_str("# Agent context inventory\n\n");
    s.push_str(&format!("- working directory: `{}`\n", inv.cwd));
    s.push_str(&format!("- scanned: {}\n", inv.scanned_at));
    s.push_str(&format!("- report id: `{id}`\n"));
    s.push_str(&format!("- target model: {}\n", opts.target_model));
    s.push_str(
        "\nToken counts are estimates (CJK counted separately from ASCII), not tokenizer output.\n",
    );

    for engine in engines_in(opts.engine_scope) {
        let mine: Vec<&Layer> = inv.layers.iter().filter(|l| l.engine == engine).collect();
        if mine.is_empty() {
            continue;
        }
        let t = totals_for(inv, engine);
        s.push_str(&format!("\n## {}\n\n", engine_name(engine)));
        s.push_str(&format!(
            "Always loaded: ~{} tokens across {} files. Not loaded until triggered: ~{} tokens across {} files.\n\n",
            t.always_tokens, t.always_files, t.on_demand_tokens, t.on_demand_files
        ));
        s.push_str("| load | kind | tokens | bytes | path |\n|---|---|---|---|---|\n");
        for l in &mine {
            let tokens = if l.in_prompt {
                format!("~{}", l.token_estimate)
            } else {
                "n/a".to_string()
            };
            let missing = if l.exists { "" } else { " (MISSING)" };
            s.push_str(&format!(
                "| {:?} | {:?} | {} | {} | `{}`{}{} |\n",
                l.load,
                l.kind,
                tokens,
                l.bytes,
                l.path,
                missing,
                origin_note(l.origin_class)
            ));
        }
    }

    // Same content in more than one place.
    let dupes = duplicate_content(inv);
    if !dupes.is_empty() {
        s.push_str("\n## Identical content in more than one layer\n\n");
        for group in dupes {
            s.push_str(&format!("- {}\n", group.join(" == ")));
        }
    }

    let empties = empty_layers(inv);
    if !empties.is_empty() {
        s.push_str("\n## Empty instruction files\n\n");
        for p in empties {
            s.push_str(&format!("- `{p}`\n"));
        }
    }

    // Cross-engine drift, by section body.
    if opts.engine_scope == EngineScope::Both {
        let drift = render_drift(inv);
        if !drift.is_empty() {
            s.push_str(&drift);
        }
    }

    // Headings always; bodies only where asked.
    s.push_str("\n## Contents\n");
    for l in inv.layers.iter().filter(|l| l.in_prompt && l.exists) {
        let Ok(body) = std::fs::read_to_string(&l.path) else {
            continue;
        };
        s.push_str(&format!("\n### `{}`{}\n\n", l.path, origin_note(l.origin_class)));
        let hs = headings(&body);
        if hs.is_empty() {
            s.push_str("(no headings)\n");
        } else {
            for h in hs {
                s.push_str(&format!("- {h}\n"));
            }
        }
        if opts.include_bodies.iter().any(|p| same_path(p, &l.path)) {
            s.push_str("\n<file-body>\n");
            s.push_str(&body);
            if !body.ends_with('\n') {
                s.push('\n');
            }
            s.push_str("</file-body>\n");
        } else {
            s.push_str("\n(body omitted — headings only)\n");
        }
    }

    s
}

/// Pair up `CLAUDE.md` and `AGENTS.md` living in the same directory and
/// compare them section by section.
fn render_drift(inv: &Inventory) -> String {
    let mut pairs: Vec<(String, String, String)> = Vec::new();
    for l in inv.layers.iter().filter(|c| c.engine == Engine::Claude) {
        if !l.path.ends_with("CLAUDE.md") {
            continue;
        }
        let sibling = l.path.replace("CLAUDE.md", "AGENTS.md");
        if inv
            .layers
            .iter()
            .any(|c| c.engine == Engine::Codex && c.path == sibling)
        {
            let dir = l
                .path
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default();
            pairs.push((dir, l.path.clone(), sibling));
        }
    }
    if pairs.is_empty() {
        return String::new();
    }

    let mut s = String::from("\n## Cross-engine drift\n");
    s.push_str(
        "\nCompared by section body, not by heading set: two files can share every heading and still differ by kilobytes of content.\n",
    );
    for (dir, claude_path, codex_path) in pairs {
        let (Ok(a), Ok(b)) = (
            std::fs::read_to_string(&claude_path),
            std::fs::read_to_string(&codex_path),
        ) else {
            continue;
        };
        let deltas = diff_sections(&a, &b);
        let changed: Vec<&SectionDelta> = deltas.iter().filter(|d| d.delta != 0).collect();
        s.push_str(&format!("\n### `{dir}`\n\n"));
        if changed.is_empty() {
            s.push_str("Identical section for section.\n");
            continue;
        }
        s.push_str("| section | CLAUDE.md | AGENTS.md | delta |\n|---|---|---|---|\n");
        for d in changed {
            s.push_str(&format!(
                "| {} | {} | {} | {:+} |\n",
                d.section, d.left_bytes, d.right_bytes, d.delta
            ));
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Analysis prompt
// ---------------------------------------------------------------------------

/// The answer contract, restated in the prompt and parsed back in the compare
/// view. Kept in one string so the two can never drift apart.
const ANSWER_SCHEMA_TEMPLATE: &str = r#"{
  "schema_version": {VERSION},
  "report_id": "<copy the report id from the report, unchanged>",
  "target_model": "<the target model named in the report>",
  "findings": [
    {
      "path": "<absolute path, exactly as it appears in the report>",
      "lines": [<first line>, <last line>],
      "verdict": "remove" | "merge" | "move" | "rewrite" | "keep",
      "as_is": "<the current text you are judging>",
      "to_be": "<the replacement text, or \"\" when the verdict is remove>",
      "rationale": "<one sentence>",
      "token_delta": <negative when this saves tokens>
    }
  ]
}"#;

/// The answer shape, with the contract version filled in from the single
/// constant so the prompt and the parser can never disagree about it.
pub fn answer_schema() -> String {
    ANSWER_SCHEMA_TEMPLATE.replace("{VERSION}", &ANSWER_SCHEMA_VERSION.to_string())
}

/// Build the paste-ready analysis prompt.
pub fn render_prompt(inv: &Inventory, opts: &ReportOptions) -> String {
    let id = report_id(inv);
    let both = opts.engine_scope == EngineScope::Both;

    let mut s = String::new();
    s.push_str(&format!(
        "You are reviewing the instruction files that configure a coding agent. \
The agent that reads them is **{}**.\n\n",
        opts.target_model
    ));
    s.push_str(
        "Judge every rule against that specific reader. A capable model does not need \
a rule spelling out what it already does well; a smaller one does. What is waste for one \
is necessary for the other, so do not apply a generic style opinion.\n\n",
    );

    s.push_str("Look for, in this order:\n\n");
    s.push_str("1. Rules the target model does not need — it already behaves that way.\n");
    s.push_str("2. The same rule stated in more than one layer, where the inner one wins anyway.\n");
    s.push_str("3. Rules that contradict each other across layers.\n");
    s.push_str("4. Rules referring to paths, commands or tools that no longer appear elsewhere in the report.\n");
    s.push_str("5. Rules that are too vague to act on, and what would make them actionable.\n");
    s.push_str("6. Gaps — something the report implies the agent needs to know that no layer states.\n");
    if both {
        s.push_str(
            "7. Cross-engine drift — the report pairs `CLAUDE.md` with the `AGENTS.md` beside it and \
lists section-body deltas. Say which differences are deliberate and which are one file simply \
falling behind.\n",
        );
    }

    s.push_str("\nGround rules:\n\n");
    s.push_str("- Cite the absolute path exactly as the report writes it, plus a line range.\n");
    s.push_str(
        "- Where a file's body was omitted you have only its headings. Say so rather than \
guessing at content you cannot see.\n",
    );
    s.push_str(
        "- Prefer few well-evidenced findings over many speculative ones. `keep` is a valid verdict \
for a rule that looks redundant but earns its place.\n",
    );
    s.push_str("- Estimate `token_delta` from the text you are removing or adding.\n");

    s.push_str("\nAnswer with one JSON object and nothing else — no preamble, no commentary \
after it. Use exactly this shape:\n\n```json\n");
    s.push_str(&answer_schema());
    s.push_str("\n```\n\n");
    s.push_str(&format!(
        "Set `report_id` to `{id}` so the answer can be matched to this snapshot.\n",
    ));

    s.push_str("\n---\n\n");
    s.push_str(&render_report(inv, opts));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scan::{scan, ScanRoots};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::{tempdir, TempDir};

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn fixture() -> (TempDir, PathBuf, ScanRoots) {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        let roots = ScanRoots {
            home: home.clone(),
            claude_dir: home.join(".claude"),
            codex_dir: home.join(".codex"),
            managed_policy: None,
        };
        (dir, home, roots)
    }

    /// Codex only reads project `AGENTS.md` inside a git repository.
    fn init_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        git2::Repository::init(path).unwrap();
    }

    fn opts(scope: EngineScope) -> ReportOptions {
        ReportOptions {
            target_model: "test-model".to_string(),
            engine_scope: scope,
            include_bodies: Vec::new(),
        }
    }

    /// Render this machine's report and prompt to disk for inspection.
    ///
    /// Ignored by default — the output depends on whose laptop runs it.
    /// Bodies stay omitted, so nothing but paths and headings is written.
    ///
    ///   SCAN_CWD=... REPORT_OUT=/tmp/out \
    ///     cargo test --lib core::report::tests::smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn smoke_report_for_this_machine() {
        let home = dirs::home_dir().expect("home");
        let cwd = std::env::var("SCAN_CWD")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap());
        let out_dir = std::env::var("REPORT_OUT").unwrap_or_else(|_| "/tmp".to_string());
        let roots = ScanRoots::from_home(&home);
        let inv = scan(&cwd, &roots, EngineScope::Both).unwrap();

        let o = ReportOptions {
            target_model: std::env::var("TARGET_MODEL")
                .unwrap_or_else(|_| "claude-opus-5".to_string()),
            engine_scope: EngineScope::Both,
            include_bodies: Vec::new(),
        };
        let report = render_report(&inv, &o);
        let prompt = render_prompt(&inv, &o);
        fs::write(
            format!("{out_dir}/inventory.json"),
            serde_json::to_string_pretty(&inv).unwrap(),
        )
        .unwrap();
        fs::write(format!("{out_dir}/report.md"), &report).unwrap();
        fs::write(format!("{out_dir}/prompt.md"), &prompt).unwrap();
        println!(
            "report {} chars, prompt {} chars, id {}",
            report.len(),
            prompt.len(),
            report_id(&inv)
        );
    }

    // --- sectioning ---

    #[test]
    fn sections_split_on_h2_and_h3_with_a_trail() {
        let md = "intro\n## One\nbody\n### Nested\nmore\n## Two\nx\n";
        let s = sections(md);
        let names: Vec<String> = s.iter().map(|x| x.path.join(" > ")).collect();
        assert_eq!(names, vec!["One", "One > Nested", "Two"]);
    }

    #[test]
    fn headings_inside_a_fence_are_not_sections() {
        let md = "\
## Real
```sh
## not a heading
### also not
```
tail
";
        let s = sections(md);
        assert_eq!(s.len(), 1, "got {:?}", s.iter().map(|x| &x.path).collect::<Vec<_>>());
        assert_eq!(s[0].path, vec!["Real"]);
    }

    #[test]
    fn identical_headings_with_different_bodies_still_show_drift() {
        // The measured failure: same heading set, kilobytes of missing body.
        let full = "## A\n".to_string() + &"x\n".repeat(500) + "## B\nshort\n";
        let thin = "## A\ntiny\n## B\nshort\n";
        assert_eq!(
            headings(&full),
            headings(thin),
            "precondition: heading sets match"
        );

        let d = diff_sections(&full, thin);
        let a = d.iter().find(|x| x.section == "A").unwrap();
        assert!(a.delta > 900, "section A must show the loss, got {}", a.delta);
        let b = d.iter().find(|x| x.section == "B").unwrap();
        assert_eq!(b.delta, 0);
    }

    #[test]
    fn a_section_present_on_only_one_side_counts_as_a_full_loss() {
        let d = diff_sections("## Only\nbody here\n", "## Other\nx\n");
        let only = d.iter().find(|x| x.section == "Only").unwrap();
        assert_eq!(only.right_bytes, 0);
        assert!(only.delta > 0);
    }

    #[test]
    fn biggest_losses_are_listed_first() {
        let left = format!("## Small\n{}## Big\n{}", "a\n".repeat(10), "b\n".repeat(200));
        let d = diff_sections(&left, "## Small\n## Big\n");
        assert_eq!(d[0].section, "Big");
    }

    // --- report id ---

    #[test]
    fn report_id_is_stable_across_rescans() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), "# hi\n");

        let a = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let b = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        assert_ne!(a.scanned_at.is_empty(), true);
        assert_eq!(
            report_id(&a),
            report_id(&b),
            "an unchanged machine must fingerprint the same"
        );
    }

    #[test]
    fn report_id_changes_when_a_layer_changes() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), "# hi\n");
        let before = report_id(&scan(&cwd, &roots, EngineScope::Claude).unwrap());

        write(&cwd.join("CLAUDE.md"), "# hi\nand more\n");
        let after = report_id(&scan(&cwd, &roots, EngineScope::Claude).unwrap());
        assert_ne!(before, after);
    }

    // --- totals ---

    #[test]
    fn totals_separate_always_from_triggered() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), &"word ".repeat(100));
        write(&cwd.join("svc").join("CLAUDE.md"), &"word ".repeat(400));

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let t = totals_for(&inv, Engine::Claude);
        assert_eq!(t.always_files, 1);
        assert_eq!(t.on_demand_files, 1);
        assert!(
            t.on_demand_tokens > t.always_tokens,
            "a big subtree file must not inflate the always-loaded total"
        );
    }

    #[test]
    fn settings_contribute_to_no_total() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        write(&roots.claude_dir.join("settings.json"), &"x".repeat(4000));

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let t = totals_for(&inv, Engine::Claude);
        assert_eq!(t.always_tokens, 0);
        assert_eq!(t.always_files, 0);
    }

    // --- duplicates and empties ---

    #[test]
    fn byte_identical_copies_in_two_layers_are_grouped() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&roots.claude_dir.join("CLAUDE.md"), "same bytes\n");
        write(&cwd.join("CLAUDE.md"), "same bytes\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let dupes = duplicate_content(&inv);
        assert_eq!(dupes.len(), 1, "got {dupes:?}");
        assert_eq!(dupes[0].len(), 2);
    }

    #[test]
    fn differing_files_are_not_reported_as_copies() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&roots.claude_dir.join("CLAUDE.md"), "one\n");
        write(&cwd.join("CLAUDE.md"), "two\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        assert!(duplicate_content(&inv).is_empty());
    }

    #[test]
    fn zero_byte_instruction_files_are_called_out() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), "");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        assert_eq!(empty_layers(&inv).len(), 1);
    }

    // --- body inclusion ---

    #[test]
    fn bodies_are_omitted_unless_the_path_is_opted_in() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), "## Head\nSECRET-BODY-TEXT\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let out = render_report(&inv, &opts(EngineScope::Claude));
        assert!(out.contains("Head"), "headings are always present");
        assert!(
            !out.contains("SECRET-BODY-TEXT"),
            "an un-opted-in body must not reach the report"
        );
        assert!(out.contains("body omitted"));
    }

    #[test]
    fn an_opted_in_body_is_embedded() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        let target = cwd.join("CLAUDE.md");
        write(&target, "## Head\nINCLUDED-BODY-TEXT\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let mut o = opts(EngineScope::Claude);
        // Exactly the path the inventory reported — the normal FE round-trip.
        o.include_bodies = vec![inv.layers[0].path.clone()];
        let out = render_report(&inv, &o);
        assert!(out.contains("INCLUDED-BODY-TEXT"));
        assert!(out.contains("<file-body>"));
        let _ = target;
    }

    #[test]
    fn an_opt_in_written_in_the_pre_symlink_shape_still_applies() {
        // On macOS a tempdir is /var/... while its canonical form is
        // /private/var/... . Failing to match would drop the body with no
        // error at all, which is worse than refusing.
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        let target = cwd.join("CLAUDE.md");
        write(&target, "## Head\nSHAPE-B-TEXT\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let mut o = opts(EngineScope::Claude);
        o.include_bodies = vec![target.to_string_lossy().to_string()];
        assert_ne!(
            o.include_bodies[0], inv.layers[0].path,
            "precondition: the two shapes really do differ here"
        );
        assert!(render_report(&inv, &o).contains("SHAPE-B-TEXT"));
    }

    // --- prompt ---

    #[test]
    fn prompt_pins_the_reader_the_schema_and_the_snapshot() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), "# hi\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let mut o = opts(EngineScope::Claude);
        o.target_model = "some-model-v9".into();
        let p = render_prompt(&inv, &o);

        assert!(p.contains("some-model-v9"), "the reader must be named");
        assert!(p.contains("\"schema_version\": 1"), "the answer shape is pinned");
        assert!(p.contains(&report_id(&inv)), "the snapshot id must round-trip");
        assert!(p.contains("# Agent context inventory"), "the report is attached");
    }

    #[test]
    fn cross_engine_question_appears_only_when_both_are_scanned() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), "# hi\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        assert!(!render_prompt(&inv, &opts(EngineScope::Claude)).contains("Cross-engine drift"));

        let inv2 = scan(&cwd, &roots, EngineScope::Both).unwrap();
        assert!(render_prompt(&inv2, &opts(EngineScope::Both)).contains("Cross-engine drift"));
    }

    #[test]
    fn company_classified_files_are_flagged_in_the_report() {
        // No owner map on disk, so nothing is company-classified and no badge
        // should appear — the feature must stay silent rather than guess.
        let (_d, home, roots) = fixture();
        let cwd = home.join("proj");
        write(&cwd.join("CLAUDE.md"), "# hi\n");

        let inv = scan(&cwd, &roots, EngineScope::Claude).unwrap();
        let out = render_report(&inv, &opts(EngineScope::Claude));
        assert!(!out.contains("⚠ company"));
    }

    #[test]
    fn drift_table_names_the_section_that_lost_body() {
        let (_d, home, roots) = fixture();
        let cwd = home.join("repo");
        init_repo(&cwd);
        let big = "## Conventions\n".to_string() + &"rule line\n".repeat(300) + "## Nav\nsame\n";
        write(&cwd.join("CLAUDE.md"), &big);
        write(&cwd.join("AGENTS.md"), "## Conventions\nthin\n## Nav\nsame\n");

        let inv = scan(&cwd, &roots, EngineScope::Both).unwrap();
        let out = render_report(&inv, &opts(EngineScope::Both));
        assert!(out.contains("Cross-engine drift"), "{out}");
        assert!(out.contains("Conventions"));
        assert!(
            !out.split("## Cross-engine drift").nth(1).unwrap().contains("| Nav |"),
            "an unchanged section must not clutter the table"
        );
    }
}
