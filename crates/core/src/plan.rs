//! Plan-title parsing and feature-branch slug derivation.

/// Extracts the text of the first level-1 Markdown heading from `markdown`.
///
/// A level-1 heading is a line that starts with `"# "`. If the extracted
/// text begins with `"Feature:"`, that prefix is stripped and the result is
/// trimmed of surrounding whitespace. Returns `None` when no such heading
/// exists.
pub fn parse_title(markdown: &str) -> Option<String> {
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            let text = rest
                .strip_prefix("Feature:")
                .unwrap_or(rest)
                .trim()
                .to_string();
            return Some(text);
        }
    }
    None
}

/// Converts `title` into a URL-safe slug.
///
/// The title is lowercased, every run of non-alphanumeric characters is
/// replaced with a single `'-'`, and leading/trailing `'-'` characters are
/// trimmed.
pub fn slugify(title: &str) -> String {
    let lower = title.to_lowercase();
    // Replace every run of non-alphanumeric chars with a single '-'.
    let mut slug = String::with_capacity(lower.len());
    let mut in_sep = false;
    for ch in lower.chars() {
        if ch.is_alphanumeric() {
            slug.push(ch);
            in_sep = false;
        } else if !in_sep {
            slug.push('-');
            in_sep = true;
        }
    }
    // Trim leading/trailing '-'.
    slug.trim_matches('-').to_string()
}

/// Returns the feature branch name for `slug`.
///
/// The result is always `"feat/<slug>"`.
pub fn branch_name(slug: &str) -> String {
    format!("feat/{slug}")
}

#[cfg(test)]
mod tests {
    use super::{branch_name, parse_title, slugify};

    #[test]
    fn parse_title_strips_feature_prefix() {
        let md = "# Feature: choragos v1 — MCP!\n\nSome body text.";
        assert_eq!(parse_title(md).as_deref(), Some("choragos v1 — MCP!"));
    }

    #[test]
    fn slugify_produces_expected_slug() {
        assert_eq!(slugify("choragos v1 — MCP!"), "choragos-v1-mcp");
    }

    #[test]
    fn branch_name_prefixes_feat() {
        assert_eq!(branch_name("choragos-v1-mcp"), "feat/choragos-v1-mcp");
    }

    #[test]
    fn parse_title_returns_none_when_no_h1() {
        let md = "## Not a level-1 heading\n\nJust some text.";
        assert_eq!(parse_title(md), None);
    }
}
