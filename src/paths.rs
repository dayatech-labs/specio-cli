//! Canonical document paths, mirroring the API's validation. Remote paths are never trusted:
//! every path that is read from the API or written locally passes through here first.
use std::path::{Path, PathBuf};

const MAX_PATH_LENGTH: usize = 300;
const MAX_SEGMENTS: usize = 8;
const ASSET_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
const EPIC_FILES: [&str; 3] = ["product-brief.md", "prd.md", "architecture.md"];
pub const FEATURE_FILES: [&str; 5] = [
    "README.md",
    "acceptance-criteria.md",
    "DESIGN.md",
    "frontend.md",
    "backend.md",
];

/// `^dr-[0-9]{2,}-[a-z0-9][a-z0-9-]{0,80}\.md$`
fn is_decision_file(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("dr-").and_then(|r| r.strip_suffix(".md")) else {
        return false;
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let Some(name) = rest[digits..].strip_prefix('-') else {
        return false;
    };
    let mut chars = name.chars();
    digits >= 2
        && matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 81
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `^[a-z0-9][a-z0-9_-]{0,63}$`
pub fn is_folder_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$`
fn is_file_segment(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && s.len() <= 100
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// True for the Markdown documents the API manages (root `llms.txt` and `README.md` included). Assets are not synced.
pub fn is_canonical_markdown(path: &str) -> bool {
    if path.is_empty()
        || path.len() > MAX_PATH_LENGTH
        || path.starts_with('/')
        || path.ends_with('/')
    {
        return false;
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return false;
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_SEGMENTS || segments.iter().any(|s| s.is_empty() || s.starts_with('.'))
    {
        return false;
    }
    let file = segments[segments.len() - 1];
    let extension = file.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if ASSET_EXTENSIONS.contains(&extension) {
        return false;
    }
    if path == "llms.txt" || path == "README.md" {
        return true;
    }
    let first = segments[0];
    if first == "design" {
        return segments.len() >= 2
            && file.len() > 3
            && file.ends_with(".md")
            && segments.iter().all(|s| is_file_segment(s));
    }
    // Epics live under `epics/<epic>/`; an epic folder at the repository root is not canonical.
    if first != "epics" || segments.len() < 3 || !is_folder_name(segments[1]) {
        return false;
    }
    match segments.len() {
        3 => EPIC_FILES.contains(&file),
        4 => segments[2] == "decisions" && is_decision_file(file),
        5 => {
            segments[2] == "features"
                && is_folder_name(segments[3])
                && FEATURE_FILES.contains(&file)
        }
        _ => false,
    }
}

/// Join a canonical `a/b/c.md` path onto a directory using the platform separator.
pub fn join_canonical(base: &Path, canonical: &str) -> PathBuf {
    canonical
        .split('/')
        .fold(base.to_path_buf(), |acc, segment| acc.join(segment))
}

/// `local_dir` must stay inside the workspace: relative, no `..`, no hidden or `.speq` segments.
pub fn is_valid_local_dir(dir: &str) -> bool {
    !dir.is_empty()
        && dir.len() <= 100
        && !dir.starts_with('/')
        && !dir.contains('\\')
        && dir.split('/').all(|s| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_documents() {
        for ok in [
            "llms.txt",
            "README.md",
            "design/tokens.md",
            "design/components/button.md",
            "epics/epic-payment/prd.md",
            "epics/epic-payment/product-brief.md",
            "epics/epic-payment/architecture.md",
            "epics/epic-payment/decisions/dr-01-use-postgres.md",
            "epics/epic-payment/features/create-payment/README.md",
            "epics/epic-payment/features/create-payment/backend.md",
        ] {
            assert!(is_canonical_markdown(ok), "{ok}");
        }
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "",
            "/etc/passwd",
            "../x.md",
            "epics/epic/../prd.md",
            "epics/epic-payment/prd.md/",
            "epics/Epic/prd.md",
            "epics/epic-payment/notes.md",
            "epics/epic-payment/features/x/notes.md",
            "epics/epic-payment/features/x/cli.md",
            "epics/epic-payment/features/x/y/README.md",
            "epics/epic-payment/decisions/use-postgres.md",
            "epics/epic-payment/decisions/dr-1-x.md",
            "epics/epic-payment/decisions/old/dr-01-x.md",
            "epics/prd.md",
            "epic-payment/prd.md",
            "epic-payment/features/x/README.md",
            ".hidden/prd.md",
            "epics/epic-payment/.prd.md",
            "epics/epic payment/prd.md",
            "epics/epic-payment/prd%2e.md",
            "epics/epic-payment\\prd.md",
            "epics/epic-payment/images/flow.png",
            "design/",
            "design/.md",
            "readme.md",
        ] {
            assert!(!is_canonical_markdown(bad), "{bad}");
        }
    }

    #[test]
    fn local_dir_rules() {
        assert!(is_valid_local_dir("specs"));
        assert!(is_valid_local_dir("docs/specs"));
        for bad in [
            "",
            "/abs",
            "../up",
            "a/../b",
            ".speq",
            "docs/.hidden",
            "a\\b",
            "a//b",
        ] {
            assert!(!is_valid_local_dir(bad), "{bad}");
        }
    }
}
