use std::collections::{BTreeMap, BTreeSet};

use super::unsafe_patch;
use crate::digest::hash_bytes;
use crate::{
    CodegenError, InstallLock, ManagedCssBlockRange, ManagedCssBlockRole, ManagedCssDependency,
    ManagedCssOperation,
};

pub fn patch_css_block(
    existing: &str,
    block_id: &str,
    block: &str,
    tracked_generated_hash: Option<&str>,
) -> Result<String, CodegenError> {
    patch_css_block_at_path(
        existing,
        "styles/kit.css",
        block_id,
        block,
        tracked_generated_hash,
    )
}

pub fn patch_css_block_at_path(
    existing: &str,
    logical_path: &str,
    block_id: &str,
    block: &str,
    tracked_generated_hash: Option<&str>,
) -> Result<String, CodegenError> {
    validate_css_block_id_at_path(block_id, logical_path)?;
    let replacement = normalize_managed_css_block_at_path(block_id, block, logical_path)?;
    let existing_block = find_managed_css_block_at_path(existing, block_id, logical_path)?;

    match existing_block {
        Some(range) => {
            let current = &existing[range.clone()];
            if current == replacement {
                return Ok(existing.to_owned());
            }

            match tracked_generated_hash {
                Some(hash) if hash_bytes(current.as_bytes()) == hash => {
                    let mut output = String::with_capacity(
                        existing.len() + replacement.len().saturating_sub(current.len()),
                    );
                    output.push_str(&existing[..range.start]);
                    output.push_str(&replacement);
                    output.push_str(&existing[range.end..]);
                    Ok(output)
                }
                Some(_) => unsafe_patch(
                    logical_path,
                    format!("managed CSS block {block_id} has local edits"),
                ),
                None => unsafe_patch(
                    logical_path,
                    format!("managed CSS block {block_id} already exists but is not tracked"),
                ),
            }
        }
        None => Ok(append_managed_css_block(existing.to_owned(), &replacement)),
    }
}

pub fn extract_managed_css_block(
    existing: &str,
    block_id: &str,
) -> Result<Option<String>, CodegenError> {
    extract_managed_css_block_at_path(existing, "styles/kit.css", block_id)
}

pub fn extract_managed_css_block_at_path(
    existing: &str,
    logical_path: &str,
    block_id: &str,
) -> Result<Option<String>, CodegenError> {
    validate_css_block_id_at_path(block_id, logical_path)?;
    Ok(
        find_managed_css_block_at_path(existing, block_id, logical_path)?
            .map(|range| existing[range].to_owned()),
    )
}

pub fn inspect_managed_css_blocks_at_path(
    existing: &str,
    logical_path: &str,
) -> Result<BTreeMap<String, ManagedCssBlockRange>, CodegenError> {
    let marker_prefix = "/* leptos-ui-kit:";
    let mut blocks = BTreeMap::new();
    let mut open: Option<(String, usize)> = None;
    let mut offset = 0;

    while let Some(relative_start) = existing[offset..].find(marker_prefix) {
        let start = offset + relative_start;
        let Some(relative_end) = existing[start..].find("*/") else {
            return unsafe_patch(logical_path, "unterminated managed CSS marker");
        };
        let marker_end = start + relative_end + 2;
        let marker = &existing[start..marker_end];
        let Some(body) = marker
            .strip_prefix(marker_prefix)
            .and_then(|marker| marker.strip_suffix(" */"))
        else {
            return unsafe_patch(
                logical_path,
                format!("malformed managed CSS marker `{marker}`"),
            );
        };
        let Some((kind, block_id)) = body.split_once(' ') else {
            return unsafe_patch(
                logical_path,
                format!("malformed managed CSS marker `{marker}`"),
            );
        };
        if block_id.contains(' ') {
            return unsafe_patch(
                logical_path,
                format!("malformed managed CSS marker `{marker}`"),
            );
        }
        validate_css_block_id_at_path(block_id, logical_path)?;

        match kind {
            "start" => {
                if let Some((open_id, _)) = &open {
                    return unsafe_patch(
                        logical_path,
                        format!(
                            "managed CSS blocks {open_id} and {block_id} overlap or are nested"
                        ),
                    );
                }
                if blocks.contains_key(block_id) {
                    return unsafe_patch(
                        logical_path,
                        format!("managed CSS block {block_id} markers are ambiguous"),
                    );
                }
                open = Some((block_id.to_owned(), start));
            }
            "end" => {
                let Some((open_id, block_start)) = open.take() else {
                    return unsafe_patch(
                        logical_path,
                        format!("managed CSS block {block_id} markers are reversed"),
                    );
                };
                if open_id != block_id {
                    return unsafe_patch(
                        logical_path,
                        format!(
                            "managed CSS blocks {open_id} and {block_id} overlap or are crossed"
                        ),
                    );
                }
                let mut block_end = marker_end;
                if existing[block_end..].starts_with('\n') {
                    block_end += 1;
                }
                blocks.insert(
                    block_id.to_owned(),
                    ManagedCssBlockRange {
                        start: block_start,
                        end: block_end,
                    },
                );
            }
            _ => {
                return unsafe_patch(
                    logical_path,
                    format!("malformed managed CSS marker `{marker}`"),
                );
            }
        }

        offset = marker_end;
    }

    if let Some((block_id, _)) = open {
        return unsafe_patch(
            logical_path,
            format!("managed CSS block {block_id} is missing its end marker"),
        );
    }

    Ok(blocks)
}

pub fn reconcile_managed_css_blocks_at_path(
    existing: &str,
    logical_path: &str,
    prior_lock: &InstallLock,
    operations: &[ManagedCssOperation],
    dependencies: &[ManagedCssDependency],
) -> Result<String, CodegenError> {
    if operations.is_empty() {
        return Ok(existing.to_owned());
    }

    let mut prepared = BTreeMap::new();
    let mut foundation_id = None;
    for (order, operation) in operations.iter().enumerate() {
        validate_css_block_id_at_path(&operation.block_id, logical_path)?;
        if operation.item_id.trim().is_empty() {
            return unsafe_patch(
                logical_path,
                "managed CSS operation has an empty item owner",
            );
        }
        let replacement = normalize_managed_css_block_at_path(
            &operation.block_id,
            &operation.generated,
            logical_path,
        )?;
        let replacement_ranges = inspect_managed_css_blocks_at_path(&replacement, logical_path)?;
        if replacement_ranges.len() != 1
            || replacement_ranges.get(&operation.block_id)
                != Some(&ManagedCssBlockRange {
                    start: 0,
                    end: replacement.len(),
                })
        {
            return unsafe_patch(
                logical_path,
                format!(
                    "generated managed CSS block {} must contain only its managed range",
                    operation.block_id
                ),
            );
        }
        if prepared
            .insert(operation.block_id.clone(), (order, operation, replacement))
            .is_some()
        {
            return unsafe_patch(
                logical_path,
                format!("duplicate managed CSS operation for {}", operation.block_id),
            );
        }
        if operation.role == ManagedCssBlockRole::Foundation
            && foundation_id.replace(operation.block_id.clone()).is_some()
        {
            return unsafe_patch(
                logical_path,
                "multiple foundation CSS operations are unsupported",
            );
        }
    }

    let mut unique_dependencies = BTreeSet::new();
    for dependency in dependencies {
        if dependency.dependency_block_id == dependency.dependent_block_id {
            return unsafe_patch(
                logical_path,
                "managed CSS dependency cannot reference itself",
            );
        }
        if !prepared.contains_key(&dependency.dependency_block_id)
            || !prepared.contains_key(&dependency.dependent_block_id)
        {
            return unsafe_patch(
                logical_path,
                format!(
                    "managed CSS dependency {} -> {} references an unknown operation",
                    dependency.dependency_block_id, dependency.dependent_block_id
                ),
            );
        }
        if !unique_dependencies.insert(dependency.clone()) {
            return unsafe_patch(
                logical_path,
                format!(
                    "duplicate managed CSS dependency {} -> {}",
                    dependency.dependency_block_id, dependency.dependent_block_id
                ),
            );
        }
    }

    let ranges = inspect_managed_css_blocks_at_path(existing, logical_path)?;
    let tracked = validate_managed_css_ownership_at_path(prior_lock, logical_path)?;

    for (block_id, range) in &ranges {
        let Some(tracked_block) = tracked.get(block_id) else {
            return unsafe_patch(
                logical_path,
                format!("managed CSS block {block_id} exists but is not tracked"),
            );
        };
        let current = &existing[range.start..range.end];
        let generated_matches = prepared
            .get(block_id)
            .is_some_and(|(_, _, replacement)| current == replacement);
        if !generated_matches && hash_bytes(current.as_bytes()) != tracked_block.generated_hash {
            return unsafe_patch(
                logical_path,
                format!("managed CSS block {block_id} has local edits"),
            );
        }
        if let Some((_, operation, _)) = prepared.get(block_id)
            && operation.item_id != tracked_block.item_id
        {
            return unsafe_patch(
                logical_path,
                format!(
                    "managed CSS block {block_id} is tracked by {} instead of {}",
                    tracked_block.item_id, operation.item_id
                ),
            );
        }
    }

    for block_id in tracked.keys() {
        if !ranges.contains_key(block_id) {
            return unsafe_patch(
                logical_path,
                format!("tracked managed CSS block {block_id} is missing"),
            );
        }
    }

    validate_css_operation_order(logical_path, &prepared, &unique_dependencies)?;

    if requires_managed_css_reorder(&prepared, &ranges, &unique_dependencies) {
        let Some(anchor) = ranges
            .iter()
            .filter(|(block_id, _)| prepared.contains_key(*block_id))
            .map(|(_, range)| range.start)
            .min()
        else {
            return unsafe_patch(
                logical_path,
                "managed CSS dependency reorder has no existing anchor",
            );
        };
        let mut ordered = String::new();
        for operation in operations {
            ordered.push_str(&prepared[&operation.block_id].2);
        }

        let mut edits = ranges
            .iter()
            .filter(|(block_id, _)| prepared.contains_key(*block_id))
            .map(|(_, range)| CssEdit::replacement(range.start, range.end, String::new()))
            .collect::<Vec<_>>();
        edits.push(CssEdit::insertion(anchor, ordered));
        let output = apply_css_edits(existing, logical_path, edits)?;
        validate_reconciled_css_order(&output, logical_path, &unique_dependencies)?;
        return Ok(output);
    }

    let mut edits = Vec::new();
    let mut missing_components = Vec::new();
    let mut foundation_insertion = None;
    let mut relocating_foundation = None;

    if let Some(block_id) = foundation_id.as_deref() {
        let (_, _, replacement) = &prepared[block_id];
        let earliest_dependent = unique_dependencies
            .iter()
            .filter(|dependency| dependency.dependency_block_id == block_id)
            .filter_map(|dependency| ranges.get(&dependency.dependent_block_id))
            .map(|range| range.start)
            .min();

        match ranges.get(block_id) {
            Some(range) => {
                let canonical_anchor = match earliest_dependent {
                    Some(anchor) if range.start > anchor => Some(anchor),
                    Some(_) => None,
                    None => Some(legal_css_preamble_end_without_range(
                        existing,
                        range,
                        logical_path,
                    )?),
                };
                if canonical_anchor.is_some_and(|anchor| anchor != range.start) {
                    foundation_insertion = Some((
                        canonical_anchor.expect("checked anchor"),
                        replacement.clone(),
                    ));
                    relocating_foundation = Some(block_id);
                }
            }
            None => {
                let anchor = match earliest_dependent {
                    Some(anchor) => anchor,
                    None => legal_css_preamble_end(existing, logical_path)?,
                };
                foundation_insertion = Some((anchor, replacement.clone()));
            }
        }
    }

    for operation in operations {
        let (_, _, replacement) = &prepared[&operation.block_id];
        match ranges.get(&operation.block_id) {
            Some(range) if relocating_foundation == Some(operation.block_id.as_str()) => {
                edits.push(CssEdit::replacement(range.start, range.end, String::new()));
            }
            Some(range) => edits.push(CssEdit::replacement(
                range.start,
                range.end,
                replacement.clone(),
            )),
            None if operation.role == ManagedCssBlockRole::Component => {
                missing_components.push(replacement.clone());
            }
            None => {}
        }
    }
    if let Some((at, replacement)) = foundation_insertion {
        edits.push(CssEdit::insertion(at, replacement));
    }

    let mut output = apply_css_edits(existing, logical_path, edits)?;

    for replacement in missing_components {
        output = append_managed_css_block(output, &replacement);
    }

    validate_reconciled_css_order(&output, logical_path, &unique_dependencies)?;
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedManagedCssBlock {
    item_id: String,
    generated_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CssEdit {
    start: usize,
    end: usize,
    replacement: String,
}

impl CssEdit {
    fn replacement(start: usize, end: usize, replacement: String) -> Self {
        Self {
            start,
            end,
            replacement,
        }
    }

    fn insertion(at: usize, replacement: String) -> Self {
        Self::replacement(at, at, replacement)
    }
}

fn validate_managed_css_ownership_at_path(
    lock: &InstallLock,
    logical_path: &str,
) -> Result<BTreeMap<String, TrackedManagedCssBlock>, CodegenError> {
    let mut tracked = BTreeMap::new();

    for (item_key, item) in &lock.items {
        for block in &item.style_blocks {
            if block.css_path != logical_path {
                return unsafe_patch(
                    logical_path,
                    format!(
                        "lock tracks managed CSS block {} at {} instead of {logical_path}",
                        block.block_id, block.css_path
                    ),
                );
            }
            if lock.style_blocks_by_id.get(&block.block_id) != Some(item_key) {
                return unsafe_patch(
                    logical_path,
                    format!(
                        "managed CSS block {} ownership does not match its lock item",
                        block.block_id
                    ),
                );
            }
            if tracked
                .insert(
                    block.block_id.clone(),
                    TrackedManagedCssBlock {
                        item_id: item_key.clone(),
                        generated_hash: block.generated_hash.clone(),
                    },
                )
                .is_some()
            {
                return unsafe_patch(
                    logical_path,
                    format!(
                        "managed CSS block {} has duplicate lock records",
                        block.block_id
                    ),
                );
            }
        }
    }

    for (block_id, item_id) in &lock.style_blocks_by_id {
        let Some(record) = tracked.get(block_id) else {
            return unsafe_patch(
                logical_path,
                format!("managed CSS block {block_id} has no owning lock record"),
            );
        };
        if &record.item_id != item_id {
            return unsafe_patch(
                logical_path,
                format!("managed CSS block {block_id} has conflicting lock owners"),
            );
        }
    }

    Ok(tracked)
}

fn validate_css_operation_order(
    logical_path: &str,
    prepared: &BTreeMap<String, (usize, &ManagedCssOperation, String)>,
    dependencies: &BTreeSet<ManagedCssDependency>,
) -> Result<(), CodegenError> {
    for dependency in dependencies {
        let (dependency_order, _, _) = &prepared[&dependency.dependency_block_id];
        let (dependent_order, _, _) = &prepared[&dependency.dependent_block_id];
        if dependency_order > dependent_order {
            return unsafe_patch(
                logical_path,
                format!(
                    "managed CSS operations are not dependency ordered: {} must precede {}",
                    dependency.dependency_block_id, dependency.dependent_block_id
                ),
            );
        }
    }
    Ok(())
}

fn requires_managed_css_reorder(
    prepared: &BTreeMap<String, (usize, &ManagedCssOperation, String)>,
    ranges: &BTreeMap<String, ManagedCssBlockRange>,
    dependencies: &BTreeSet<ManagedCssDependency>,
) -> bool {
    dependencies.iter().any(|dependency| {
        let (_, dependency_operation, _) = &prepared[&dependency.dependency_block_id];
        if dependency_operation.role == ManagedCssBlockRole::Foundation {
            return false;
        }
        match (
            ranges.get(&dependency.dependency_block_id),
            ranges.get(&dependency.dependent_block_id),
        ) {
            (Some(dependency_range), Some(dependent_range)) => {
                dependency_range.start > dependent_range.start
            }
            (None, Some(_)) => true,
            _ => false,
        }
    })
}

fn apply_css_edits(
    existing: &str,
    logical_path: &str,
    mut edits: Vec<CssEdit>,
) -> Result<String, CodegenError> {
    edits.sort_by_key(|edit| (edit.start, usize::from(edit.start != edit.end)));
    let mut output = String::with_capacity(existing.len());
    let mut cursor = 0;
    for edit in edits {
        if edit.start < cursor || edit.end < edit.start || edit.end > existing.len() {
            return unsafe_patch(logical_path, "managed CSS edit ranges overlap");
        }
        output.push_str(&existing[cursor..edit.start]);
        output.push_str(&edit.replacement);
        cursor = edit.end;
    }
    output.push_str(&existing[cursor..]);
    Ok(output)
}

fn validate_reconciled_css_order(
    reconciled: &str,
    logical_path: &str,
    dependencies: &BTreeSet<ManagedCssDependency>,
) -> Result<(), CodegenError> {
    let ranges = inspect_managed_css_blocks_at_path(reconciled, logical_path)?;
    for dependency in dependencies {
        let Some(dependency_range) = ranges.get(&dependency.dependency_block_id) else {
            return unsafe_patch(
                logical_path,
                format!(
                    "managed CSS block {} is missing",
                    dependency.dependency_block_id
                ),
            );
        };
        let Some(dependent_range) = ranges.get(&dependency.dependent_block_id) else {
            return unsafe_patch(
                logical_path,
                format!(
                    "managed CSS block {} is missing",
                    dependency.dependent_block_id
                ),
            );
        };
        if dependency_range.start > dependent_range.start {
            return unsafe_patch(
                logical_path,
                format!(
                    "managed CSS dependency {} must precede {}",
                    dependency.dependency_block_id, dependency.dependent_block_id
                ),
            );
        }
    }
    Ok(())
}

fn append_managed_css_block(mut existing: String, replacement: &str) -> String {
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    if !existing.trim().is_empty() {
        existing.push('\n');
    }
    existing.push_str(replacement);
    existing
}

fn legal_css_preamble_end(existing: &str, logical_path: &str) -> Result<usize, CodegenError> {
    let mut cursor = usize::from(existing.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();

    loop {
        cursor = consume_css_preamble_trivia(existing, cursor, logical_path)?;
        let Some(keyword) = ["@charset", "@import", "@namespace"]
            .into_iter()
            .find(|keyword| css_keyword_at(existing, cursor, keyword))
        else {
            return Ok(cursor);
        };
        cursor =
            scan_css_preamble_statement(existing, cursor + keyword.len(), keyword, logical_path)?;
    }
}

fn legal_css_preamble_end_without_range(
    existing: &str,
    range: &ManagedCssBlockRange,
    logical_path: &str,
) -> Result<usize, CodegenError> {
    let mut without_foundation = String::with_capacity(existing.len() - (range.end - range.start));
    without_foundation.push_str(&existing[..range.start]);
    without_foundation.push_str(&existing[range.end..]);
    let anchor = legal_css_preamble_end(&without_foundation, logical_path)?;

    Ok(if anchor <= range.start {
        anchor
    } else {
        anchor + (range.end - range.start)
    })
}

fn consume_css_preamble_trivia(
    existing: &str,
    mut cursor: usize,
    logical_path: &str,
) -> Result<usize, CodegenError> {
    while cursor < existing.len() {
        if existing[cursor..].starts_with("/* leptos-ui-kit:") {
            break;
        }
        if existing[cursor..].starts_with("/*") {
            let Some(relative_end) = existing[cursor + 2..].find("*/") else {
                return unsafe_patch(logical_path, "unterminated comment in CSS preamble");
            };
            cursor += 2 + relative_end + 2;
            continue;
        }
        if existing.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        break;
    }
    Ok(cursor)
}

fn css_keyword_at(existing: &str, cursor: usize, keyword: &str) -> bool {
    let Some(candidate) = existing.get(cursor..cursor.saturating_add(keyword.len())) else {
        return false;
    };
    if !candidate.eq_ignore_ascii_case(keyword) {
        return false;
    }
    existing
        .as_bytes()
        .get(cursor + keyword.len())
        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_'))
}

fn scan_css_preamble_statement(
    existing: &str,
    mut cursor: usize,
    keyword: &str,
    logical_path: &str,
) -> Result<usize, CodegenError> {
    let mut quote = None;
    let mut parentheses = 0usize;

    while cursor < existing.len() {
        let byte = existing.as_bytes()[cursor];
        if let Some(delimiter) = quote {
            match byte {
                b'\\' => {
                    cursor += 1;
                    if cursor == existing.len() {
                        return unsafe_patch(
                            logical_path,
                            format!("unterminated escape in {keyword} CSS preamble statement"),
                        );
                    }
                    cursor = next_char_boundary(existing, cursor);
                }
                byte if byte == delimiter => {
                    quote = None;
                    cursor += 1;
                }
                b'\n' | b'\r' => {
                    return unsafe_patch(
                        logical_path,
                        format!("unterminated string in {keyword} CSS preamble statement"),
                    );
                }
                _ => cursor = next_char_boundary(existing, cursor),
            }
            continue;
        }

        if existing[cursor..].starts_with("/*") {
            let Some(relative_end) = existing[cursor + 2..].find("*/") else {
                return unsafe_patch(
                    logical_path,
                    format!("unterminated comment in {keyword} CSS preamble statement"),
                );
            };
            cursor += 2 + relative_end + 2;
            continue;
        }

        match byte {
            b'\'' | b'"' => {
                quote = Some(byte);
                cursor += 1;
            }
            b'\\' => {
                cursor += 1;
                if cursor == existing.len() {
                    return unsafe_patch(
                        logical_path,
                        format!("unterminated escape in {keyword} CSS preamble statement"),
                    );
                }
                cursor = next_char_boundary(existing, cursor);
            }
            b'(' => {
                parentheses += 1;
                cursor += 1;
            }
            b')' => {
                let Some(next) = parentheses.checked_sub(1) else {
                    return unsafe_patch(
                        logical_path,
                        format!("unbalanced parentheses in {keyword} CSS preamble statement"),
                    );
                };
                parentheses = next;
                cursor += 1;
            }
            b';' if parentheses == 0 => return Ok(cursor + 1),
            b'{' | b'}' if parentheses == 0 => {
                return unsafe_patch(
                    logical_path,
                    format!("unterminated {keyword} CSS preamble statement"),
                );
            }
            _ => cursor = next_char_boundary(existing, cursor),
        }
    }

    let reason = if quote.is_some() {
        format!("unterminated string in {keyword} CSS preamble statement")
    } else if parentheses != 0 {
        format!("unbalanced parentheses in {keyword} CSS preamble statement")
    } else {
        format!("unterminated {keyword} CSS preamble statement")
    };
    unsafe_patch(logical_path, reason)
}

fn next_char_boundary(value: &str, cursor: usize) -> usize {
    cursor
        + value[cursor..]
            .chars()
            .next()
            .expect("cursor is before the end of a UTF-8 string")
            .len_utf8()
}

fn normalize_managed_css_block_at_path(
    block_id: &str,
    block: &str,
    logical_path: &str,
) -> Result<String, CodegenError> {
    let start_marker = css_start_marker(block_id);
    let end_marker = css_end_marker(block_id);

    if block.matches(&start_marker).count() != 1 || block.matches(&end_marker).count() != 1 {
        return unsafe_patch(
            logical_path,
            format!("managed CSS block {block_id} must contain exactly one start and end marker"),
        );
    }

    let Some(start) = block.find(&start_marker) else {
        return unsafe_patch(
            logical_path,
            format!("managed CSS block {block_id} is missing its start marker"),
        );
    };
    let Some(end) = block.find(&end_marker) else {
        return unsafe_patch(
            logical_path,
            format!("managed CSS block {block_id} is missing its end marker"),
        );
    };
    if start > end {
        return unsafe_patch(
            logical_path,
            format!("managed CSS block {block_id} markers are reversed"),
        );
    }

    let mut normalized = block.trim_matches('\n').to_owned();
    normalized.push('\n');
    Ok(normalized)
}

fn find_managed_css_block_at_path(
    existing: &str,
    block_id: &str,
    logical_path: &str,
) -> Result<Option<std::ops::Range<usize>>, CodegenError> {
    let start_marker = css_start_marker(block_id);
    let end_marker = css_end_marker(block_id);
    let start_count = existing.matches(&start_marker).count();
    let end_count = existing.matches(&end_marker).count();

    match (start_count, end_count) {
        (0, 0) => Ok(None),
        (1, 1) => {
            let start = existing.find(&start_marker).expect("count confirmed start");
            let end_start = existing.find(&end_marker).expect("count confirmed end");
            if start > end_start {
                return unsafe_patch(
                    logical_path,
                    format!("managed CSS block {block_id} markers are reversed"),
                );
            }
            let mut end = end_start + end_marker.len();
            if existing[end..].starts_with('\n') {
                end += 1;
            }
            Ok(Some(start..end))
        }
        _ => unsafe_patch(
            logical_path,
            format!("managed CSS block {block_id} markers are ambiguous"),
        ),
    }
}

fn css_start_marker(block_id: &str) -> String {
    format!("/* leptos-ui-kit:start {block_id} */")
}

fn css_end_marker(block_id: &str) -> String {
    format!("/* leptos-ui-kit:end {block_id} */")
}

fn validate_css_block_id_at_path(block_id: &str, logical_path: &str) -> Result<(), CodegenError> {
    if block_id.is_empty()
        || !block_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return unsafe_patch(
            logical_path,
            "CSS block id must be lowercase ASCII, digits, or hyphens",
        );
    }
    Ok(())
}
