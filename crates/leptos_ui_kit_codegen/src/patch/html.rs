use std::fmt;

use leptos_ui_kit_registry::KitConfig;
use serde::{Deserialize, Serialize};

use crate::path_safety::PlanningContext;
use crate::planning::push_file_plan;
use crate::{ChangeKind, ChangeRecord, CodegenError, PlannedFile, PlannedFileAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlHeadInspection {
    pub start_tag: HtmlSpan,
    pub content: HtmlSpan,
    pub end_tag: HtmlSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlLinkInspection {
    pub tag: HtmlSpan,
    pub inside_head: bool,
    pub data_trunk: bool,
    pub rel_css: bool,
    pub href: Option<String>,
}

impl HtmlLinkInspection {
    pub fn is_active_trunk_css(&self) -> bool {
        self.data_trunk && self.rel_css
    }

    pub fn matches_stylesheet(&self, stylesheet_path: &str) -> bool {
        self.inside_head
            && self.is_active_trunk_css()
            && self.href.as_deref() == Some(stylesheet_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlInspection {
    pub head: HtmlHeadInspection,
    pub links: Vec<HtmlLinkInspection>,
}

impl HtmlInspection {
    pub fn matching_stylesheet_links(
        &self,
        stylesheet_path: &str,
    ) -> impl Iterator<Item = &HtmlLinkInspection> {
        self.links
            .iter()
            .filter(move |link| link.matches_stylesheet(stylesheet_path))
    }

    pub fn first_head_trunk_css_link(&self) -> Option<&HtmlLinkInspection> {
        self.links
            .iter()
            .find(|link| link.inside_head && link.is_active_trunk_css())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HtmlInspectionError {
    MissingHead,
    DuplicateHead { offset: usize },
    UnexpectedHeadEnd { offset: usize },
    UnclosedHead { offset: usize },
    Malformed { offset: usize, reason: String },
}

impl fmt::Display for HtmlInspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHead => formatter.write_str("HTML document is missing a head element"),
            Self::DuplicateHead { offset } => {
                write!(
                    formatter,
                    "HTML document has a duplicate head element at byte {offset}"
                )
            }
            Self::UnexpectedHeadEnd { offset } => {
                write!(
                    formatter,
                    "HTML document has an unmatched </head> at byte {offset}"
                )
            }
            Self::UnclosedHead { offset } => {
                write!(
                    formatter,
                    "HTML head element opened at byte {offset} is not closed"
                )
            }
            Self::Malformed { offset, reason } => {
                write!(formatter, "malformed HTML at byte {offset}: {reason}")
            }
        }
    }
}

impl std::error::Error for HtmlInspectionError {}

#[derive(Debug)]
struct ParsedTag {
    span: HtmlSpan,
    name: String,
    end: bool,
    self_closing: bool,
    attributes: Vec<ParsedAttribute>,
}

#[derive(Debug)]
struct ParsedAttribute {
    name: String,
    value: Option<String>,
}

/// Inspects only byte spans and decoded attribute values; it never
/// reserializes or normalizes app-owned HTML.
pub fn inspect_html(input: &str) -> Result<HtmlInspection, HtmlInspectionError> {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    let mut head_open: Option<(HtmlSpan, usize)> = None;
    let mut head: Option<HtmlHeadInspection> = None;
    let mut links = Vec::new();

    while cursor < bytes.len() {
        if bytes[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        if bytes[cursor..].starts_with(b"<!--") {
            let Some(relative_end) = input[cursor + 4..].find("-->") else {
                return malformed(cursor, "unterminated comment");
            };
            cursor += 4 + relative_end + 3;
            continue;
        }
        if bytes[cursor..].starts_with(b"<!") || bytes[cursor..].starts_with(b"<?") {
            cursor = declaration_end(input, cursor)?;
            continue;
        }
        let Some(next) = bytes.get(cursor + 1) else {
            return malformed(cursor, "unterminated tag opener");
        };
        if !next.is_ascii_alphabetic() && *next != b'/' {
            cursor += 1;
            continue;
        }

        let tag = parse_tag(input, cursor)?;
        cursor = tag.span.end;
        if tag.name.eq_ignore_ascii_case("head") {
            if tag.end {
                let Some((start_tag, content_start)) = head_open.take() else {
                    return Err(HtmlInspectionError::UnexpectedHeadEnd {
                        offset: tag.span.start,
                    });
                };
                head = Some(HtmlHeadInspection {
                    start_tag,
                    content: HtmlSpan {
                        start: content_start,
                        end: tag.span.start,
                    },
                    end_tag: tag.span,
                });
            } else {
                if head.is_some() || head_open.is_some() {
                    return Err(HtmlInspectionError::DuplicateHead {
                        offset: tag.span.start,
                    });
                }
                if tag.self_closing {
                    return malformed(tag.span.start, "head element cannot be self-closing");
                }
                head_open = Some((tag.span, tag.span.end));
            }
            continue;
        }

        if !tag.end && tag.name.eq_ignore_ascii_case("link") {
            links.push(inspect_link(&tag, head_open.is_some())?);
        }

        if !tag.end
            && !tag.self_closing
            && (tag.name.eq_ignore_ascii_case("script") || tag.name.eq_ignore_ascii_case("style"))
        {
            cursor = find_raw_text_end(input, cursor, &tag.name).ok_or_else(|| {
                HtmlInspectionError::Malformed {
                    offset: tag.span.start,
                    reason: format!("unclosed {} raw-text element", tag.name),
                }
            })?;
        }
    }

    if let Some((start, _)) = head_open {
        return Err(HtmlInspectionError::UnclosedHead {
            offset: start.start,
        });
    }
    let head = head.ok_or(HtmlInspectionError::MissingHead)?;
    Ok(HtmlInspection { head, links })
}

fn inspect_link(
    tag: &ParsedTag,
    inside_head: bool,
) -> Result<HtmlLinkInspection, HtmlInspectionError> {
    let data_trunk = attribute(tag, "data-trunk").is_some();
    let rel_css = attribute(tag, "rel")
        .and_then(|attribute| attribute.value.as_deref())
        .is_some_and(|value| {
            value
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("css"))
        });
    let href = attribute(tag, "href")
        .and_then(|attribute| attribute.value.as_deref())
        .map(|value| decode_character_references(value, tag.span.start))
        .transpose()?;
    Ok(HtmlLinkInspection {
        tag: tag.span,
        inside_head,
        data_trunk,
        rel_css,
        href,
    })
}

fn attribute<'a>(tag: &'a ParsedTag, name: &str) -> Option<&'a ParsedAttribute> {
    tag.attributes
        .iter()
        .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
}

fn parse_tag(input: &str, start: usize) -> Result<ParsedTag, HtmlInspectionError> {
    let bytes = input.as_bytes();
    let mut cursor = start + 1;
    let end = bytes.get(cursor) == Some(&b'/');
    if end {
        cursor += 1;
    }
    skip_ascii_whitespace(bytes, &mut cursor);
    let name_start = cursor;
    while bytes.get(cursor).is_some_and(|byte| is_name_byte(*byte)) {
        cursor += 1;
    }
    if cursor == name_start {
        return malformed(start, "tag name is missing");
    }
    let name = input[name_start..cursor].to_ascii_lowercase();
    let mut attributes = Vec::new();
    let mut self_closing = false;

    loop {
        skip_ascii_whitespace(bytes, &mut cursor);
        match bytes.get(cursor).copied() {
            Some(b'>') => {
                cursor += 1;
                break;
            }
            Some(b'/') if bytes.get(cursor + 1) == Some(&b'>') => {
                if end {
                    return malformed(cursor, "end tag cannot be self-closing");
                }
                self_closing = true;
                cursor += 2;
                break;
            }
            None => return malformed(start, "unterminated tag"),
            _ if end => return malformed(cursor, "end tag contains attributes"),
            _ => {}
        }

        let attribute_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| is_attribute_name_byte(*byte))
        {
            cursor += 1;
        }
        if cursor == attribute_start {
            return malformed(cursor, "invalid attribute name");
        }
        let attribute_name = input[attribute_start..cursor].to_ascii_lowercase();
        if attributes
            .iter()
            .any(|attribute: &ParsedAttribute| attribute.name == attribute_name)
        {
            return malformed(attribute_start, "duplicate attribute");
        }
        skip_ascii_whitespace(bytes, &mut cursor);
        let value = if bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
            skip_ascii_whitespace(bytes, &mut cursor);
            Some(parse_attribute_value(input, &mut cursor)?)
        } else {
            None
        };
        attributes.push(ParsedAttribute {
            name: attribute_name,
            value,
        });
    }

    Ok(ParsedTag {
        span: HtmlSpan { start, end: cursor },
        name,
        end,
        self_closing,
        attributes,
    })
}

fn parse_attribute_value(input: &str, cursor: &mut usize) -> Result<String, HtmlInspectionError> {
    let bytes = input.as_bytes();
    match bytes.get(*cursor).copied() {
        Some(quote @ (b'"' | b'\'')) => {
            let quote_offset = *cursor;
            *cursor += 1;
            let value_start = *cursor;
            while bytes.get(*cursor).is_some_and(|byte| *byte != quote) {
                *cursor += 1;
            }
            if bytes.get(*cursor) != Some(&quote) {
                return malformed(quote_offset, "unterminated quoted attribute value");
            }
            let value = input[value_start..*cursor].to_owned();
            *cursor += 1;
            Ok(value)
        }
        Some(b'>') | None => malformed(*cursor, "attribute value is missing"),
        Some(_) => {
            let value_start = *cursor;
            while bytes.get(*cursor).is_some_and(|byte| {
                !byte.is_ascii_whitespace() && !matches!(*byte, b'>' | b'<' | b'"' | b'\'' | b'=')
            }) {
                *cursor += 1;
            }
            if *cursor == value_start {
                return malformed(value_start, "invalid unquoted attribute value");
            }
            Ok(input[value_start..*cursor].to_owned())
        }
    }
}

fn declaration_end(input: &str, start: usize) -> Result<usize, HtmlInspectionError> {
    let bytes = input.as_bytes();
    let mut cursor = start + 2;
    let mut quote = None;
    while let Some(byte) = bytes.get(cursor).copied() {
        match (quote, byte) {
            (Some(expected), actual) if expected == actual => quote = None,
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'>') => return Ok(cursor + 1),
            _ => {}
        }
        cursor += 1;
    }
    malformed(start, "unterminated declaration")
}

fn find_raw_text_end(input: &str, mut cursor: usize, name: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    while cursor < bytes.len() {
        let relative = input[cursor..].find('<')?;
        cursor += relative;
        if bytes.get(cursor + 1) == Some(&b'/') {
            let name_start = cursor + 2;
            let name_end = name_start.checked_add(name.len())?;
            if input
                .get(name_start..name_end)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
                && bytes
                    .get(name_end)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>')
            {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

fn decode_character_references(
    value: &str,
    tag_offset: usize,
) -> Result<String, HtmlInspectionError> {
    let mut decoded = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative) = value[cursor..].find('&') {
        let amp = cursor + relative;
        decoded.push_str(&value[cursor..amp]);
        let Some(relative_end) = value[amp + 1..].find(';') else {
            decoded.push_str(&value[amp..]);
            return Ok(decoded);
        };
        let end = amp + 1 + relative_end;
        let reference = &value[amp + 1..end];
        let character = match reference {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            _ if reference.starts_with("#x") || reference.starts_with("#X") => {
                u32::from_str_radix(&reference[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if reference.starts_with('#') => {
                reference[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        if reference.starts_with('#') && character.is_none() {
            return malformed(tag_offset + amp, "invalid numeric character reference");
        }
        if let Some(character) = character {
            decoded.push(character);
        } else {
            decoded.push_str(&value[amp..=end]);
        }
        cursor = end + 1;
    }
    decoded.push_str(&value[cursor..]);
    Ok(decoded)
}

fn skip_ascii_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':' | b'_')
}

fn is_attribute_name_byte(byte: u8) -> bool {
    is_name_byte(byte) || byte == b'.'
}

fn malformed<T>(offset: usize, reason: impl Into<String>) -> Result<T, HtmlInspectionError> {
    Err(HtmlInspectionError::Malformed {
        offset,
        reason: reason.into(),
    })
}

pub(crate) fn plan_index_html(
    context: &PlanningContext,
    files: &mut Vec<PlannedFile>,
    changes: &mut Vec<ChangeRecord>,
    config: &KitConfig,
) -> Result<(), CodegenError> {
    let path = context.project_root().join("index.html");
    let html = context.read_string("index.html")?;
    let css_path = config.styles.css.as_str();
    if contains_trunk_css_link(&html, css_path) {
        return Ok(());
    }

    let Some(head_end) = html.find("</head>") else {
        return Err(CodegenError::UnsafePatch {
            path,
            reason: "missing </head> marker".to_owned(),
        });
    };

    if html.matches("<head").count() != 1 || html.matches("</head>").count() != 1 {
        return Err(CodegenError::UnsafePatch {
            path,
            reason: "ambiguous head element".to_owned(),
        });
    }

    let insert_at = first_head_trunk_css_link_index(&html, head_end).unwrap_or(head_end);
    let indent = line_indent_at(&html, insert_at).unwrap_or("    ");
    let link = format!("{indent}<link data-trunk rel=\"css\" href=\"{css_path}\" />\n");

    let mut patched = html;
    patched.insert_str(insert_at, &link);

    push_file_plan(
        files,
        changes,
        "index.html",
        PlannedFileAction::Update,
        patched,
        ChangeKind::UpdateFile,
    );
    Ok(())
}

fn contains_trunk_css_link(html: &str, css_path: &str) -> bool {
    html.lines().any(|line| {
        line.contains("data-trunk")
            && line.contains("rel=\"css\"")
            && line.contains(&format!("href=\"{css_path}\""))
    })
}

fn first_head_trunk_css_link_index(html: &str, head_end: usize) -> Option<usize> {
    let mut offset = 0;
    for line in html.split_inclusive('\n') {
        if offset >= head_end {
            return None;
        }
        if line.contains("data-trunk") && line.contains("rel=\"css\"") {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

fn line_indent_at(html: &str, index: usize) -> Option<&str> {
    let line = html.get(index..)?.lines().next()?;
    let indent_len = line
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    line.get(..indent_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_recognizes_lossless_attribute_and_entity_variants() {
        let html = r#"<!DOCTYPE html>
<HTML><HeAd>
  <LiNk href='styles/kit&amp;theme.css' REL="preload CSS" DATA-TRUNK>
  <link data-trunk rel=css href=styles/other.css>
</HeAd><body></body></HTML>"#;

        let inspection = inspect_html(html).expect("inspect");

        assert_eq!(inspection.links.len(), 2);
        assert!(
            inspection.links[0].matches_stylesheet("styles/kit&theme.css"),
            "quoted href character references are decoded"
        );
        assert!(
            inspection.links[1].matches_stylesheet("styles/other.css"),
            "unquoted attributes are recognized"
        );
        assert_eq!(
            &html[inspection.head.start_tag.start..inspection.head.start_tag.end],
            "<HeAd>"
        );
        assert_eq!(
            &html[inspection.head.end_tag.start..inspection.head.end_tag.end],
            "</HeAd>"
        );
    }

    #[test]
    fn scanner_ignores_comments_and_script_or_style_raw_text() {
        let html = r#"<html><head>
<!-- <link data-trunk rel="css" href="styles/kit.css"> -->
<script>const fake = '<link data-trunk rel="css" href="styles/kit.css">';</script>
<style>.fake::after { content: "<link data-trunk rel='css'>"; }</style>
<link data-trunk rel="css" href="styles/real.css">
</head><body></body></html>"#;

        let inspection = inspect_html(html).expect("inspect");

        assert_eq!(inspection.links.len(), 1);
        assert!(inspection.links[0].matches_stylesheet("styles/real.css"));
    }

    #[test]
    fn scanner_distinguishes_head_and_body_links() {
        let html = r#"<html><head>
<link data-trunk rel="css" href="styles/head.css">
</head><body>
<link data-trunk rel="css" href="styles/body.css">
</body></html>"#;

        let inspection = inspect_html(html).expect("inspect");

        assert!(inspection.links[0].inside_head);
        assert!(!inspection.links[1].inside_head);
        assert_eq!(
            inspection
                .matching_stylesheet_links("styles/body.css")
                .count(),
            0
        );
        assert_eq!(
            inspection
                .first_head_trunk_css_link()
                .map(|link| link.href.as_deref()),
            Some(Some("styles/head.css"))
        );
    }

    #[test]
    fn scanner_preserves_exact_one_line_lf_and_crlf_spans() {
        for html in [
            "<html><head></head><body></body></html>",
            "<html>\n<head>\n</head>\n</html>\n",
            "<html>\r\n<head>\r\n</head>\r\n</html>\r\n",
        ] {
            let inspection = inspect_html(html).expect("inspect");
            assert_eq!(
                &html[inspection.head.start_tag.start..inspection.head.start_tag.end],
                "<head>"
            );
            assert_eq!(
                &html[inspection.head.end_tag.start..inspection.head.end_tag.end],
                "</head>"
            );
            assert_eq!(inspection.head.content.start, inspection.head.start_tag.end);
            assert_eq!(inspection.head.content.end, inspection.head.end_tag.start);
        }
    }

    #[test]
    fn scanner_rejects_missing_duplicate_unclosed_and_unmatched_heads() {
        assert_eq!(
            inspect_html("<html><body></body></html>"),
            Err(HtmlInspectionError::MissingHead)
        );
        assert!(matches!(
            inspect_html("<head></head><head></head>"),
            Err(HtmlInspectionError::DuplicateHead { .. })
        ));
        assert!(matches!(
            inspect_html("<html><head><body></body></html>"),
            Err(HtmlInspectionError::UnclosedHead { .. })
        ));
        assert!(matches!(
            inspect_html("<html></head></html>"),
            Err(HtmlInspectionError::UnexpectedHeadEnd { .. })
        ));
    }

    #[test]
    fn scanner_rejects_malformed_comments_tags_quotes_and_raw_text() {
        for html in [
            "<head><!--</head>",
            "<head><link href=\"styles/kit.css></head>",
            "<head><link =bad></head>",
            "<head><script>const x = 1;</head>",
            "<head><style>.x {}</head>",
            "<head><link href='a' HREF='b'></head>",
        ] {
            assert!(
                matches!(
                    inspect_html(html),
                    Err(HtmlInspectionError::Malformed { .. })
                ),
                "{html}"
            );
        }
    }

    #[test]
    fn scanner_decodes_named_decimal_and_hex_href_references() {
        let html = r#"<head>
<link data-trunk rel="alternate css" href="styles/kit&#46;&#x63;ss?x=1&amp;y=2">
</head>"#;
        let inspection = inspect_html(html).expect("inspect");

        assert!(inspection.links[0].matches_stylesheet("styles/kit.css?x=1&y=2"));
    }

    #[test]
    fn scanner_terminates_on_large_pathological_text() {
        let mut html = "<".repeat(1_000_000);
        html.push_str("<head></head>");

        let inspection = inspect_html(&html).expect("bounded scan");

        assert_eq!(inspection.head.content.start, inspection.head.content.end);
    }
}
