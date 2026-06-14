//! Active `CLAUDE.md` composition (v0.3 — Connected Context).
//!
//! v0.1 toggled the active file by copying ONE flat profile onto it, so drift
//! detection could compare the active file byte-for-byte against that profile
//! (`core::drift`, `core::profile_store::detect_active`). v0.3 layers up to two
//! *modifier* regions on top of a base profile body:
//!
//! ```text
//! <base profile body>
//!
//! <!--toggler:domains:start-->
//! ## Active Domain Docs
//! @domains/kubernetes/INDEX.md
//! <!--toggler:domains:end-->
//!
//! <!--toggler:awareness:start-->
//! ## Domain Awareness
//! - Kubernetes: low — expand acronyms + external links
//! <!--toggler:awareness:end-->
//! ```
//!
//! - The **domains** region (21-2) is a set of `@import` pointers to selected
//!   domain doc-trees.
//! - The **awareness** region (#19 / v0.2 domain-level mode) is reserved here so
//!   that when v0.2 is built it injects its block through this single composer
//!   instead of appending independently — two independent "append to the end"
//!   writers would corrupt the sentinel boundaries.
//!
//! The composed bytes are written to a RESERVED baseline file `{target}.composed`
//! and then applied to the active target through `ToggleEngine`, preserving the
//! atomic-write / advisory-lock / origin-backup invariants exactly. That baseline
//! file is what `check_drift` compares the composed active file against — without
//! it the composed file would match no flat profile and drift would fire on every
//! FileWatcher tick.

use std::fs;
use std::io;

use thiserror::Error;

use crate::core::profile_store::COMPOSED_NAME;
use crate::core::toggle_engine::{ToggleEngine, ToggleError};

const DOMAINS_START: &str = "<!--toggler:domains:start-->";
const DOMAINS_END: &str = "<!--toggler:domains:end-->";
const AWARENESS_START: &str = "<!--toggler:awareness:start-->";
const AWARENESS_END: &str = "<!--toggler:awareness:end-->";

const MANAGED_NOTE: &str =
    "<!-- managed by claude-md-toggler — edits inside this block are overwritten on Apply -->";

#[derive(Debug, Error)]
pub enum ComposeError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("toggle error: {0}")]
    Toggle(#[from] ToggleError),
}

/// True when `content` carries any toggler-managed modifier region.
pub fn has_blocks(content: &str) -> bool {
    content.contains(DOMAINS_START) || content.contains(AWARENESS_START)
}

/// Remove every toggler-managed region (inclusive of its sentinel lines) from
/// `content`, returning the pristine base body with a single trailing newline.
///
/// Line-based and idempotent: stripping already-stripped content is a no-op.
/// Used to (a) recover a pristine base before re-composing and (b) capture a
/// clean origin backup that never contains toggler blocks.
pub fn strip_blocks(content: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in content.lines() {
        let t = line.trim();
        if t == DOMAINS_START || t == AWARENESS_START {
            skipping = true;
            continue;
        }
        if t == DOMAINS_END || t == AWARENESS_END {
            skipping = false;
            continue;
        }
        if !skipping {
            out.push(line);
        }
    }
    // Drop trailing blank lines left behind by a removed block, then normalize
    // to exactly one trailing newline.
    while out.last().map_or(false, |l| l.trim().is_empty()) {
        out.pop();
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn render_domains_block(domain_imports: &[String]) -> Option<String> {
    if domain_imports.is_empty() {
        return None;
    }
    let mut s = String::new();
    s.push_str(DOMAINS_START);
    s.push_str("\n## Active Domain Docs\n");
    s.push_str(MANAGED_NOTE);
    s.push('\n');
    for line in domain_imports {
        s.push_str(line);
        s.push('\n');
    }
    s.push_str(DOMAINS_END);
    Some(s)
}

fn render_awareness_block(block: Option<&str>) -> Option<String> {
    let block = block?;
    let trimmed = block.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut s = String::new();
    s.push_str(AWARENESS_START);
    s.push('\n');
    s.push_str(trimmed);
    s.push('\n');
    s.push_str(AWARENESS_END);
    Some(s)
}

/// Compose the active file body from a base profile body plus optional modifier
/// regions. `domain_imports` are `@import` lines (21-2); `awareness_block` is the
/// rendered #19 domain-awareness body (without sentinels). The base is always
/// stripped of any stale toggler blocks first so re-composition is stable.
///
/// Region order is fixed `[base][domains][awareness]` so the sentinel layout is
/// deterministic regardless of which modifiers are present.
pub fn compose(base_body: &str, domain_imports: &[String], awareness_block: Option<&str>) -> String {
    let base = strip_blocks(base_body);
    let base = base.trim_end_matches('\n');
    let mut out = String::from(base);
    for block in [
        render_domains_block(domain_imports),
        render_awareness_block(awareness_block),
    ]
    .into_iter()
    .flatten()
    {
        out.push_str("\n\n");
        out.push_str(&block);
    }
    out.push('\n');
    out
}

/// Compose `base_body` with the given modifiers, write the result to the reserved
/// `{target}.composed` baseline, and atomically apply it to the active target via
/// `ToggleEngine`.
///
/// A pristine origin backup is captured *before* the first composed write so the
/// backup can never end up holding toggler-managed blocks (origin must stay a
/// clean pre-toggler snapshot). The caller is responsible for setting the
/// `active_is_composed` flag and updating the drift baseline name afterwards.
pub fn compose_and_apply(
    engine: &ToggleEngine,
    base_body: &str,
    domain_imports: &[String],
    awareness_block: Option<&str>,
) -> Result<String, ComposeError> {
    ensure_pristine_origin(engine)?;

    let composed = compose(base_body, domain_imports, awareness_block);
    let composed_path = engine.profile_path(COMPOSED_NAME);
    fs::write(&composed_path, &composed)?;
    engine.apply_profile(&composed_path)?;
    Ok(composed)
}

/// If no origin backup exists yet, capture one from the *pristine* (block-stripped)
/// current target. This guards against a fresh-machine sequence where the first
/// action is a composed apply: `ToggleEngine::ensure_backup` would otherwise
/// snapshot whatever is in the target at that moment, and if that already carried
/// toggler blocks the origin would be poisoned. Idempotent — never overwrites an
/// existing origin (matching `ensure_backup` semantics).
fn ensure_pristine_origin(engine: &ToggleEngine) -> Result<(), ComposeError> {
    if engine.backup().exists() {
        return Ok(());
    }
    let target = engine.target();
    if !target.exists() {
        return Ok(());
    }
    let current = fs::read_to_string(target)?;
    let pristine = strip_blocks(&current);
    fs::write(engine.backup(), pristine)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn imports(ids: &[&str]) -> Vec<String> {
        ids.iter()
            .map(|id| format!("@domains/{}/INDEX.md", id))
            .collect()
    }

    #[test]
    fn compose_base_only_when_no_modifiers() {
        let out = compose("# base\nbody line\n", &[], None);
        assert_eq!(out, "# base\nbody line\n");
        assert!(!has_blocks(&out));
    }

    #[test]
    fn compose_adds_domains_block() {
        let out = compose("# base\n", &imports(&["kubernetes", "trading"]), None);
        assert!(out.contains(DOMAINS_START));
        assert!(out.contains("## Active Domain Docs"));
        assert!(out.contains("@domains/kubernetes/INDEX.md"));
        assert!(out.contains("@domains/trading/INDEX.md"));
        assert!(out.contains(DOMAINS_END));
        assert!(!out.contains(AWARENESS_START));
    }

    #[test]
    fn compose_orders_domains_before_awareness() {
        let out = compose("# base\n", &imports(&["k8s"]), Some("- Spring: high"));
        let d = out.find(DOMAINS_START).unwrap();
        let a = out.find(AWARENESS_START).unwrap();
        assert!(d < a, "domains region must precede awareness region");
        assert!(out.contains("- Spring: high"));
    }

    #[test]
    fn compose_is_idempotent_over_recomposition() {
        let once = compose("# base\n", &imports(&["k8s"]), Some("- K8s: low"));
        // Feeding a composed body back in must strip the old blocks and rebuild
        // the same result — never nest sentinels.
        let twice = compose(&once, &imports(&["k8s"]), Some("- K8s: low"));
        assert_eq!(once, twice);
        assert_eq!(once.matches(DOMAINS_START).count(), 1);
        assert_eq!(once.matches(AWARENESS_START).count(), 1);
    }

    #[test]
    fn strip_blocks_recovers_pristine_base() {
        let composed = compose("# base\nkeep me\n", &imports(&["k8s"]), Some("- x: low"));
        let stripped = strip_blocks(&composed);
        assert_eq!(stripped, "# base\nkeep me\n");
        assert!(!has_blocks(&stripped));
    }

    #[test]
    fn empty_domains_and_awareness_yield_base() {
        let out = compose("# base\n", &[], Some("   "));
        assert_eq!(out, "# base\n");
    }

    #[test]
    fn compose_and_apply_writes_composed_baseline_and_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        fs::write(&target, "# pristine base\n").unwrap();
        let engine = ToggleEngine::new(target.clone());

        let composed = compose_and_apply(
            &engine,
            "# pristine base\n",
            &imports(&["kubernetes"]),
            None,
        )
        .unwrap();

        // Active target now holds the composed bytes.
        assert_eq!(fs::read_to_string(&target).unwrap(), composed);
        // The composed baseline file exists and matches — this is what drift
        // detection compares against.
        let baseline = dir.path().join("CLAUDE.md.composed");
        assert_eq!(fs::read_to_string(&baseline).unwrap(), composed);
        assert!(composed.contains("@domains/kubernetes/INDEX.md"));
    }

    #[test]
    fn compose_and_apply_captures_pristine_origin_not_composed() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        fs::write(&target, "# my real claude md\n").unwrap();
        let engine = ToggleEngine::new(target.clone());

        // First-ever action is a composed apply (fresh-machine scenario).
        compose_and_apply(&engine, "# my real claude md\n", &imports(&["k8s"]), None).unwrap();

        // Origin must be the pristine pre-toggler content, with NO toggler blocks.
        let origin = fs::read_to_string(dir.path().join("CLAUDE.md.origin")).unwrap();
        assert_eq!(origin, "# my real claude md\n");
        assert!(!has_blocks(&origin));
    }

    #[test]
    fn ensure_pristine_origin_strips_blocks_from_already_composed_target() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        // Simulate a crash that left a composed target with no origin yet.
        let stale = compose("# base\n", &imports(&["k8s"]), None);
        fs::write(&target, &stale).unwrap();
        let engine = ToggleEngine::new(target);

        ensure_pristine_origin(&engine).unwrap();
        let origin = fs::read_to_string(dir.path().join("CLAUDE.md.origin")).unwrap();
        assert_eq!(origin, "# base\n", "origin must be the stripped pristine base");
    }

    #[test]
    fn ensure_pristine_origin_is_noop_when_origin_exists() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let origin = dir.path().join("CLAUDE.md.origin");
        fs::write(&target, "current\n").unwrap();
        fs::write(&origin, "PRESERVE ME\n").unwrap();
        let engine = ToggleEngine::new(target);

        ensure_pristine_origin(&engine).unwrap();
        assert_eq!(
            fs::read_to_string(&origin).unwrap(),
            "PRESERVE ME\n",
            "existing origin must never be overwritten"
        );
    }
}
