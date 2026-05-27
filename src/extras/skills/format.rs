//! Skill format validation and frontmatter parsing.
//!
//! Skills are stored as directories under `.dirge/skills/` with a
//! `SKILL.md` file. The file starts with YAML frontmatter (between
//! `---` delimiters) followed by Markdown body content.

/// A parsed skill specification — the in-memory representation of
/// a `SKILL.md` file with its frontmatter metadata extracted.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillSpec {
    /// Skill name (lowercase, hyphens, max 64 chars). From
    /// frontmatter `name:` field or the directory name.
    pub name: String,
    /// Human-readable description from frontmatter `description:`.
    pub description: String,
    /// The full file content (frontmatter + body).
    pub content: String,
    /// Tags extracted from `tags:` in frontmatter dirge metadata.
    pub tags: Vec<String>,
    /// Related skill names from `related_skills:` in metadata.
    pub related: Vec<String>,
    /// The body content (everything after the closing `---`).
    pub body: String,
}

// ── Validation constants ───────────────────────────────

/// Maximum length of a skill name in bytes. 256 bytes is plenty
/// for UTF-8 identifiers (e.g. ~85 CJK code points) while still
/// being a sane upper bound that bounds memory and prevents abuse.
const MAX_NAME_LEN: usize = 256;

/// Maximum total content size (100K chars ≈ 36K tokens).
const MAX_CONTENT_LEN: usize = 100_000;

// ── Public API ─────────────────────────────────────────

/// Parse a `SKILL.md` file's content into a [`SkillSpec`]. Uses
/// `dir_name` as the fallback name when frontmatter omits it.
pub fn parse_skill_spec(content: &str, dir_name: &str) -> Option<SkillSpec> {
    let (frontmatter, body) = split_frontmatter(content)?;
    let body = body.trim().to_string();
    if body.is_empty() {
        return None;
    }

    let yaml = parse_yaml_frontmatter(&frontmatter);

    let name = yaml
        .scalar("name")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| dir_name.to_string());

    let description = yaml.scalar("description").unwrap_or_default();

    // Tags / related can live either at the top level or nested
    // under `metadata.dirge.*`. Try both — top level wins.
    let tags = yaml
        .list("tags")
        .or_else(|| yaml.list_at_path(&["metadata", "dirge", "tags"]))
        .unwrap_or_default();
    let related = yaml
        .list("related_skills")
        .or_else(|| yaml.list_at_path(&["metadata", "dirge", "related_skills"]))
        .unwrap_or_default();

    Some(SkillSpec {
        name,
        description,
        content: content.to_string(),
        tags,
        related,
        body,
    })
}

/// Validate a skill name. Returns `Ok(())` if the name is valid,
/// `Err(reason)` otherwise.
///
/// Rules (loosened to support real-world names):
/// - non-empty
/// - ≤ MAX_NAME_LEN bytes
/// - no path separators (`/`, `\`)
/// - no null bytes or other control chars (via `char::is_control`)
/// - must not start with `.` (would conflict with dotfiles)
///
/// Otherwise any Unicode letters, digits, hyphens, dots, etc.
/// are accepted — `kebab-case`, `skill.v2`, `日本語スキル` all OK.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Skill name must not be empty".to_string());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "Skill name too long ({} bytes, max {})",
            name.len(),
            MAX_NAME_LEN
        ));
    }
    if name.starts_with('.') {
        return Err("Skill name must not start with '.'".to_string());
    }
    for c in name.chars() {
        if c == '/' || c == '\\' {
            return Err("Skill name must not contain path separators".to_string());
        }
        if c.is_control() {
            return Err("Skill name must not contain control characters".to_string());
        }
    }
    Ok(())
}

/// Validate total content size. Returns error if over the limit.
pub fn validate_content_size(content: &str) -> Result<(), String> {
    if content.len() > MAX_CONTENT_LEN {
        return Err(format!(
            "Skill content too large ({} chars, max {})",
            content.len(),
            MAX_CONTENT_LEN
        ));
    }
    Ok(())
}

/// Build the frontmatter header for a skill.
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_frontmatter(name: &str, description: &str, tags: &[String]) -> String {
    let mut fm = String::from("---\n");
    fm.push_str(&format!("name: {}\n", name));
    if !description.is_empty() {
        fm.push_str(&format!("description: {}\n", description));
    }
    if !tags.is_empty() {
        fm.push_str("metadata:\n");
        fm.push_str("  dirge:\n");
        fm.push_str("    tags: [");
        fm.push_str(
            &tags
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        fm.push_str("]\n");
    }
    fm.push_str("---\n\n");
    fm
}

// ── Internal helpers ───────────────────────────────────

/// Split frontmatter from body. Returns `None` if there's no
/// frontmatter or it's malformed. Returns `(frontmatter_text, body_text)`.
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let content = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;

    let (fm, body) = if let Some(pos) = content.find("\n---") {
        let (a, b) = content.split_at(pos);
        (a.to_string(), b[4..].to_string())
    } else if let Some(pos) = content.find("\r\n---") {
        let (a, b) = content.split_at(pos);
        (a.to_string(), b[5..].to_string())
    } else {
        return None;
    };

    Some((fm, body))
}

// ── Minimal YAML frontmatter parser ────────────────────
//
// A real `serde_yaml` integration would be the obvious solution
// but the project is intentionally lean on dependencies (see
// Cargo.toml — minimalist coding agent). `serde_yaml` is also
// unmaintained since 2024.
//
// This hand-rolled parser covers the cases we documented:
//   - scalar values, optionally quoted with `"…"` or `'…'`
//     (handles embedded `:` inside quotes)
//   - block scalars `|` and `>` (multi-line strings)
//   - flow arrays `[a, b, c]`
//   - flow maps  `{ key: value, key2: value2 }`
//   - nested block maps via indentation (2-space convention)
//   - block list items `- item`
//
// We don't model the full YAML 1.2 spec — we model the shape
// of skill frontmatter we actually see.

/// A minimal YAML value — sum of scalar / sequence / mapping.
#[derive(Debug, Clone, PartialEq)]
enum YamlValue {
    Scalar(String),
    Sequence(Vec<YamlValue>),
    Mapping(Vec<(String, YamlValue)>),
}

impl YamlValue {
    /// Get a child value by key (only meaningful for mappings).
    fn get(&self, key: &str) -> Option<&YamlValue> {
        if let YamlValue::Mapping(entries) = self {
            for (k, v) in entries {
                if k == key {
                    return Some(v);
                }
            }
        }
        None
    }

    /// Get a scalar string at the top-level key.
    fn scalar(&self, key: &str) -> Option<String> {
        match self.get(key)? {
            YamlValue::Scalar(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Get a list-of-strings at the top-level key. Returns `None`
    /// if the key is missing; returns `Some(vec![])` if it's
    /// present but empty.
    fn list(&self, key: &str) -> Option<Vec<String>> {
        match self.get(key)? {
            YamlValue::Sequence(items) => Some(
                items
                    .iter()
                    .filter_map(|v| match v {
                        YamlValue::Scalar(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            YamlValue::Scalar(s) if !s.is_empty() => Some(vec![s.clone()]),
            _ => None,
        }
    }

    /// Walk a path of keys and return a list-of-strings.
    fn list_at_path(&self, path: &[&str]) -> Option<Vec<String>> {
        let (last, rest) = path.split_last()?;
        let mut cur = self;
        for k in rest {
            cur = cur.get(k)?;
        }
        cur.list(last)
    }
}

/// Parse frontmatter body (between the `---` markers) into a
/// top-level mapping. Anything that doesn't parse cleanly is
/// silently skipped — we want the parser to be best-effort, not
/// a YAML linter.
fn parse_yaml_frontmatter(frontmatter: &str) -> YamlValue {
    // Strip blank/comment lines, but keep indentation on real
    // lines so we can detect block structure.
    let lines: Vec<&str> = frontmatter
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .collect();
    let (value, _consumed) = parse_block_mapping(&lines, 0, 0);
    value
}

/// Count leading-space indent (tabs count as 1 — we don't see them
/// in our frontmatter and treating them as 1 keeps the math simple).
fn indent_of(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

/// Parse a block mapping starting at `lines[start]` with the given
/// indent. Returns the mapping plus the number of lines consumed.
fn parse_block_mapping(lines: &[&str], start: usize, indent: usize) -> (YamlValue, usize) {
    let mut entries: Vec<(String, YamlValue)> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        let line_indent = indent_of(line);
        if line_indent < indent {
            break;
        }
        if line_indent > indent {
            // Shouldn't happen at this level — skip defensively.
            i += 1;
            continue;
        }
        let trimmed = &line[line_indent..];
        // A block list item at this indent terminates the map.
        if trimmed.starts_with("- ") || trimmed == "-" {
            break;
        }
        let Some((key, rest)) = split_key_value(trimmed) else {
            // Not a mapping line — give up on this level.
            break;
        };
        i += 1;
        let rest_trim = rest.trim();
        // Block scalar markers `|` / `|-` / `>` / `>-`.
        if rest_trim == "|"
            || rest_trim == "|-"
            || rest_trim == "|+"
            || rest_trim == ">"
            || rest_trim == ">-"
            || rest_trim == ">+"
        {
            let fold = rest_trim.starts_with('>');
            let (text, consumed) = parse_block_scalar(lines, i, indent, fold);
            entries.push((key, YamlValue::Scalar(text)));
            i += consumed;
            continue;
        }
        if !rest_trim.is_empty() {
            // Scalar / flow value on the same line.
            entries.push((key, parse_inline_value(rest_trim)));
            continue;
        }
        // Empty value — look at the next line. May be a nested
        // mapping or a block sequence.
        if i < lines.len() {
            let next = lines[i];
            let next_indent = indent_of(next);
            if next_indent > indent {
                let next_trim = &next[next_indent..];
                if next_trim.starts_with("- ") || next_trim == "-" {
                    let (seq, consumed) = parse_block_sequence(lines, i, next_indent);
                    entries.push((key, seq));
                    i += consumed;
                } else {
                    let (sub, consumed) = parse_block_mapping(lines, i, next_indent);
                    entries.push((key, sub));
                    i += consumed;
                }
                continue;
            }
        }
        // No value, no children — empty scalar.
        entries.push((key, YamlValue::Scalar(String::new())));
    }
    (YamlValue::Mapping(entries), i - start)
}

/// Parse a block sequence at the given indent.
fn parse_block_sequence(lines: &[&str], start: usize, indent: usize) -> (YamlValue, usize) {
    let mut items: Vec<YamlValue> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        let line_indent = indent_of(line);
        if line_indent < indent {
            break;
        }
        if line_indent > indent {
            i += 1;
            continue;
        }
        let trimmed = &line[line_indent..];
        let Some(item_text) = trimmed
            .strip_prefix("- ")
            .or_else(|| if trimmed == "-" { Some("") } else { None })
        else {
            break;
        };
        i += 1;
        let item_text = item_text.trim();
        if item_text.is_empty() {
            // Possibly a nested mapping at greater indent.
            if i < lines.len() {
                let next_indent = indent_of(lines[i]);
                if next_indent > indent {
                    let (sub, consumed) = parse_block_mapping(lines, i, next_indent);
                    items.push(sub);
                    i += consumed;
                    continue;
                }
            }
            items.push(YamlValue::Scalar(String::new()));
        } else {
            items.push(parse_inline_value(item_text));
        }
    }
    (YamlValue::Sequence(items), i - start)
}

/// Parse a block scalar (literal `|` or folded `>`). Returns the
/// joined text plus number of lines consumed.
fn parse_block_scalar(
    lines: &[&str],
    start: usize,
    parent_indent: usize,
    fold: bool,
) -> (String, usize) {
    let mut content_lines: Vec<&str> = Vec::new();
    let mut i = start;
    let mut block_indent: Option<usize> = None;
    while i < lines.len() {
        let line = lines[i];
        let line_indent = indent_of(line);
        if line_indent <= parent_indent {
            break;
        }
        let indent = *block_indent.get_or_insert(line_indent);
        // Strip the block's indent off the front, defensively.
        let stripped = if line.len() >= indent {
            &line[indent..]
        } else {
            ""
        };
        content_lines.push(stripped);
        i += 1;
    }
    let joined = if fold {
        content_lines.join(" ")
    } else {
        content_lines.join("\n")
    };
    (joined.trim_end().to_string(), i - start)
}

/// Split a line `key: value` into `(key, value-text)`. Handles
/// quoted keys minimally — most skill frontmatter uses bare keys.
/// Skips `:` chars inside `"…"` / `'…'` quotes so values like
/// `description: "foo: bar"` work correctly.
fn split_key_value(line: &str) -> Option<(String, &str)> {
    let bytes = line.as_bytes();
    let mut in_double = false;
    let mut in_single = false;
    let mut idx = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' if !in_single => in_double = !in_double,
            b'\'' if !in_double => in_single = !in_single,
            b':' if !in_double && !in_single => {
                // Require either end-of-line or a following space —
                // matches YAML's mapping-indicator rule.
                let next = bytes.get(i + 1).copied();
                if next.is_none() || next == Some(b' ') || next == Some(b'\t') {
                    idx = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let idx = idx?;
    let key_raw = line[..idx].trim();
    let key = strip_quotes(key_raw).to_string();
    if key.is_empty() {
        return None;
    }
    Some((key, &line[idx + 1..]))
}

/// Strip a surrounding `"…"` or `'…'` quote pair.
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Parse a single-line YAML value. Recognises `[…]` flow arrays,
/// `{…}` flow maps, quoted strings, or bare scalars.
fn parse_inline_value(s: &str) -> YamlValue {
    let s = s.trim();
    if s.starts_with('[') && s.ends_with(']') {
        return YamlValue::Sequence(
            split_flow(&s[1..s.len() - 1])
                .into_iter()
                .map(|item| parse_inline_value(&item))
                .collect(),
        );
    }
    if s.starts_with('{') && s.ends_with('}') {
        let entries = split_flow(&s[1..s.len() - 1])
            .into_iter()
            .filter_map(|item| {
                let (k, v) = split_key_value(&item)?;
                Some((k, parse_inline_value(v.trim())))
            })
            .collect();
        return YamlValue::Mapping(entries);
    }
    YamlValue::Scalar(strip_quotes(s).to_string())
}

/// Split flow-style content (inside `[…]` or `{…}`) by top-level
/// commas, respecting nested brackets and quotes.
fn split_flow(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut in_double = false;
    let mut in_single = false;
    let mut start = 0;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' if !in_single => in_double = !in_double,
            b'\'' if !in_double => in_single = !in_single,
            b'[' | b'{' if !in_double && !in_single => depth += 1,
            b']' | b'}' if !in_double && !in_single => depth -= 1,
            b',' if depth == 0 && !in_double && !in_single => {
                items.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_string());
    }
    items.into_iter().filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_name ─────────────────────────────────

    #[test]
    fn valid_name_passes() {
        assert!(validate_name("project-build").is_ok());
        assert!(validate_name("rust-best-practices").is_ok());
        assert!(validate_name("a").is_ok());
    }

    #[test]
    fn empty_name_rejected() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn uppercase_name_accepted() {
        // Loosened validation: mixed case is allowed.
        assert!(validate_name("Project-Build").is_ok());
    }

    #[test]
    fn underscore_and_space_accepted() {
        // Loosened: underscores and spaces are both legal Unicode
        // identifier-ish characters and not banned by the new
        // rules. Path separators and control chars are still
        // rejected (see dedicated tests below).
        assert!(validate_name("project_build").is_ok());
        assert!(validate_name("project build").is_ok());
    }

    #[test]
    fn leading_trailing_hyphen_accepted() {
        // Loosened: only `.` at the start is forbidden.
        assert!(validate_name("-project").is_ok());
        assert!(validate_name("project-").is_ok());
    }

    #[test]
    fn too_long_name_rejected() {
        // The cap is in bytes — 257 ASCII bytes > 256.
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert!(validate_name(&long).is_err());
    }

    #[test]
    fn skill_name_accepts_unicode() {
        assert!(validate_name("日本語スキル").is_ok());
        assert!(validate_name("café-skill").is_ok());
    }

    #[test]
    fn skill_name_accepts_dots_after_first_char() {
        assert!(validate_name("skill.v2").is_ok());
        assert!(validate_name("a.b.c").is_ok());
    }

    #[test]
    fn skill_name_rejects_path_separator() {
        assert!(validate_name("foo/bar").is_err());
        assert!(validate_name("foo\\bar").is_err());
    }

    #[test]
    fn skill_name_rejects_control_chars() {
        assert!(validate_name("foo\x01bar").is_err());
        assert!(validate_name("foo\0bar").is_err());
        assert!(validate_name("foo\nbar").is_err());
    }

    #[test]
    fn skill_name_rejects_leading_dot() {
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name(".").is_err());
    }

    // ── parse_skill_spec ──────────────────────────────

    #[test]
    fn parse_valid_skill() {
        let content = "---\nname: project-build\ndescription: Build commands\n---\n\nRun `cargo build` to compile.\n";
        let spec = parse_skill_spec(content, "fallback").unwrap();
        assert_eq!(spec.name, "project-build");
        assert_eq!(spec.description, "Build commands");
        assert!(spec.body.contains("cargo build"));
    }

    #[test]
    fn parse_falls_back_to_dir_name() {
        let content = "---\ndescription: no name field\n---\n\nbody here\n";
        let spec = parse_skill_spec(content, "dir-name").unwrap();
        assert_eq!(spec.name, "dir-name");
    }

    #[test]
    fn parse_rejects_empty_body() {
        let content = "---\nname: test\n---\n   \n";
        assert!(parse_skill_spec(content, "dir").is_none());
    }

    #[test]
    fn parse_no_frontmatter_returns_none() {
        assert!(parse_skill_spec("just body", "dir").is_none());
    }

    #[test]
    fn parse_extracts_tags() {
        let content =
            "---\nname: s\nmetadata:\n  dirge:\n    tags: [build, rust, cargo]\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "s").unwrap();
        assert_eq!(spec.tags, vec!["build", "rust", "cargo"]);
    }

    #[test]
    fn frontmatter_with_empty_name_defaults_to_dir() {
        let content = "---\nname:\ndescription: desc\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "dir-name").unwrap();
        assert_eq!(spec.name, "dir-name");
    }

    // ── validate_content_size ─────────────────────────

    #[test]
    fn content_size_under_limit() {
        assert!(validate_content_size("short").is_ok());
    }

    #[test]
    fn content_size_over_limit() {
        let big = "x".repeat(100_001);
        assert!(validate_content_size(&big).is_err());
    }

    // ── build_frontmatter ─────────────────────────────

    #[test]
    fn build_frontmatter_includes_name_and_description() {
        let fm = build_frontmatter("my-skill", "Does things", &[]);
        assert!(fm.contains("name: my-skill"));
        assert!(fm.contains("description: Does things"));
        assert!(fm.starts_with("---\n"));
        assert!(fm.ends_with("---\n\n"));
    }

    #[test]
    fn build_frontmatter_includes_tags() {
        let fm = build_frontmatter("s", "", &["rust".into(), "build".into()]);
        assert!(fm.contains("tags: [rust, build]"));
    }

    // ── YAML frontmatter parser ───────────────────────

    #[test]
    fn yaml_empty_list_for_missing_key() {
        let yaml = parse_yaml_frontmatter("name: foo\n");
        assert!(yaml.list("tags").is_none());
    }

    #[test]
    fn yaml_single_scalar_promoted_to_list() {
        let yaml = parse_yaml_frontmatter("tags: rust\n");
        assert_eq!(yaml.list("tags"), Some(vec!["rust".to_string()]));
    }

    #[test]
    fn parse_skill_spec_handles_multi_line_description() {
        let content =
            "---\nname: s\ndescription: |\n  Multi-line text\n  continues here\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "s").unwrap();
        assert_eq!(spec.description, "Multi-line text\ncontinues here");
    }

    #[test]
    fn parse_skill_spec_handles_folded_description() {
        let content = "---\nname: s\ndescription: >\n  First line\n  second line\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "s").unwrap();
        assert_eq!(spec.description, "First line second line");
    }

    #[test]
    fn parse_skill_spec_handles_nested_map() {
        // `tools: { allowed: [read], denied: [write] }` — flow map.
        // The parser must descend without choking; we don't surface
        // tools in `SkillSpec`, but adjacent fields must still parse.
        let content = "---\nname: s\ntools: { allowed: [read], denied: [write] }\ndescription: ok\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "s").unwrap();
        assert_eq!(spec.name, "s");
        assert_eq!(spec.description, "ok");
    }

    #[test]
    fn parse_skill_spec_handles_flow_array() {
        let content = "---\nname: s\ntags: [a, b, c]\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "s").unwrap();
        assert_eq!(spec.tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn parse_skill_spec_handles_quoted_string_with_colon() {
        let content = "---\nname: s\ndescription: \"foo: bar: baz\"\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "s").unwrap();
        assert_eq!(spec.description, "foo: bar: baz");
    }

    #[test]
    fn parse_skill_spec_handles_block_list() {
        let content = "---\nname: s\ntags:\n  - alpha\n  - beta\n  - gamma\n---\n\nbody\n";
        let spec = parse_skill_spec(content, "s").unwrap();
        assert_eq!(spec.tags, vec!["alpha", "beta", "gamma"]);
    }
}
