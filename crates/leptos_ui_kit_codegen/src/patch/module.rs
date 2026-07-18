use std::{ops::Range, path::Path};

use super::unsafe_patch;
use crate::{CodegenError, UiModuleExport};

type GroupedPubUseRanges = Vec<(Range<usize>, Vec<String>)>;

pub fn patch_components_mod(existing: Option<&str>) -> Result<String, CodegenError> {
    patch_module_lines(
        existing.unwrap_or_default(),
        "src/components/mod.rs",
        &["pub mod ui;"],
    )
}

pub fn patch_ui_mod(
    existing: Option<&str>,
    exports: &[UiModuleExport],
) -> Result<String, CodegenError> {
    let mut lines = Vec::new();

    for export in exports {
        validate_patch_identifier(
            &export.module,
            "UI module name",
            Path::new("src/components/ui/mod.rs"),
        )?;
        validate_module_path(
            &export.path,
            "UI export path",
            Path::new("src/components/ui/mod.rs"),
        )?;
        for symbol in &export.symbols {
            validate_patch_identifier(
                symbol,
                "UI export symbol",
                Path::new("src/components/ui/mod.rs"),
            )?;
        }

        lines.push(format!("pub mod {};", export.module));
        if !export.symbols.is_empty() {
            if let [symbol] = export.symbols.as_slice() {
                lines.push(format!("pub use {}::{};", export.path, symbol));
            } else {
                lines.push(format_grouped_pub_use(&export.path, &export.symbols));
            }
        }
    }

    let borrowed = lines.iter().map(String::as_str).collect::<Vec<_>>();
    patch_module_lines(
        existing.unwrap_or_default(),
        "src/components/ui/mod.rs",
        &borrowed,
    )
}

fn patch_module_lines(
    existing: &str,
    logical_path: &str,
    required_lines: &[&str],
) -> Result<String, CodegenError> {
    let mut output = existing.to_owned();

    for line in required_lines {
        if line.trim() != *line || line.is_empty() {
            return unsafe_patch(logical_path, "module patch line must be normalized");
        }
        if let Some(patched) = consolidate_grouped_pub_use(&output, line)? {
            output = patched;
            continue;
        }
        if module_line_exists(&output, line)? {
            continue;
        }
        if detects_private_module_conflict(&output, line) {
            return unsafe_patch(
                logical_path,
                format!("private module declaration conflicts with required line `{line}`"),
            );
        }
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(line);
        output.push('\n');
    }

    Ok(output)
}

fn consolidate_grouped_pub_use(
    existing: &str,
    required_line: &str,
) -> Result<Option<String>, CodegenError> {
    let Some((path, required_symbols)) = parse_grouped_pub_use(required_line)? else {
        return Ok(None);
    };
    let grouped_ranges = grouped_pub_use_ranges(existing, path)?;
    let single_ranges = single_pub_use_ranges(existing, path)?;
    if grouped_ranges.is_empty() && single_ranges.is_empty() {
        return Ok(None);
    }
    let symbols = required_symbols
        .iter()
        .map(|symbol| (*symbol).to_owned())
        .collect::<Vec<_>>();
    if grouped_ranges.len() == 1 && single_ranges.is_empty() && grouped_ranges[0].1 == symbols {
        return Ok(None);
    }

    let replacement = format_grouped_pub_use(path, &symbols);
    let mut ranges = grouped_ranges
        .into_iter()
        .map(|(range, _)| range)
        .chain(single_ranges.into_iter().map(|(range, _)| range))
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| range.start);

    let mut output = String::new();
    let mut last = 0;
    for (index, range) in ranges.iter().enumerate() {
        output.push_str(&existing[last..range.start]);
        if index == 0 {
            output.push_str(&replacement);
            if existing[range.clone()].ends_with('\n') && !replacement.ends_with('\n') {
                output.push('\n');
            }
        }
        last = range.end;
    }
    output.push_str(&existing[last..]);

    Ok(Some(output))
}

fn single_pub_use_ranges(
    existing: &str,
    path: &str,
) -> Result<Vec<(Range<usize>, String)>, CodegenError> {
    let prefix = format!("pub use {path}::");
    let mut ranges = Vec::new();
    let mut offset = 0;

    for line in existing.split_inclusive('\n') {
        let line_start = offset;
        let line_end = line_start + line.len();
        offset = line_end;

        let trimmed = line.trim();
        let Some(symbol) = trimmed
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(';'))
        else {
            continue;
        };
        if symbol.contains('{') || symbol.contains(',') {
            continue;
        }

        validate_patch_identifier(
            symbol,
            "UI export symbol",
            Path::new("src/components/ui/mod.rs"),
        )?;
        ranges.push((line_start..line_end, symbol.to_owned()));
    }

    Ok(ranges)
}

fn module_line_exists(existing: &str, required_line: &str) -> Result<bool, CodegenError> {
    if existing
        .lines()
        .any(|existing_line| existing_line.trim() == required_line)
    {
        return Ok(true);
    }

    let Some((path, symbols)) = parse_grouped_pub_use(required_line)? else {
        return Ok(false);
    };

    let marker = format!("pub use {path}::{{");
    let mut offset = 0;
    while let Some(relative_start) = existing[offset..].find(&marker) {
        let start = offset + relative_start + marker.len();
        let Some(relative_end) = existing[start..].find("};") else {
            return Ok(false);
        };
        let end = start + relative_end;
        if grouped_pub_use_contains(&existing[start..end], &symbols) {
            return Ok(true);
        }
        offset = end + 2;
    }

    Ok(false)
}

fn grouped_pub_use_ranges(existing: &str, path: &str) -> Result<GroupedPubUseRanges, CodegenError> {
    let marker = format!("pub use {path}::{{");
    let mut ranges = Vec::new();
    let mut offset = 0;
    while let Some(relative_start) = existing[offset..].find(&marker) {
        let start = offset + relative_start;
        let body_start = start + marker.len();
        let Some(relative_end) = existing[body_start..].find("};") else {
            break;
        };
        let body_end = body_start + relative_end;
        let end = body_end + 2;
        let symbols = existing[body_start..body_end]
            .split(',')
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty())
            .map(|symbol| {
                validate_patch_identifier(
                    symbol,
                    "UI export symbol",
                    Path::new("src/components/ui/mod.rs"),
                )?;
                Ok(symbol.to_owned())
            })
            .collect::<Result<Vec<_>, CodegenError>>()?;
        ranges.push((start..end, symbols));
        offset = end;
    }

    Ok(ranges)
}

fn parse_grouped_pub_use(required_line: &str) -> Result<Option<(&str, Vec<&str>)>, CodegenError> {
    let Some(body) = required_line
        .strip_prefix("pub use ")
        .and_then(|line| line.strip_suffix("};"))
    else {
        return Ok(None);
    };
    let Some((path, symbols)) = body.split_once("::{") else {
        return Ok(None);
    };
    validate_module_path(
        path,
        "UI export path",
        Path::new("src/components/ui/mod.rs"),
    )?;
    let symbols = symbols
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .collect::<Vec<_>>();
    if symbols.is_empty() {
        return Ok(None);
    }
    for symbol in &symbols {
        validate_patch_identifier(
            symbol,
            "UI export symbol",
            Path::new("src/components/ui/mod.rs"),
        )?;
    }

    Ok(Some((path, symbols)))
}

fn grouped_pub_use_contains(existing_symbols: &str, required_symbols: &[&str]) -> bool {
    let existing_symbols = existing_symbols
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .collect::<Vec<_>>();

    required_symbols
        .iter()
        .all(|symbol| existing_symbols.iter().any(|existing| existing == symbol))
}

fn format_grouped_pub_use(path: &str, symbols: &[String]) -> String {
    let one_line = format!("pub use {}::{{{}}};", path, symbols.join(", "));
    if one_line.len() <= 100 {
        return one_line;
    }

    let mut output = format!("pub use {path}::{{\n");
    let mut line = String::from("    ");
    for symbol in symbols {
        let next = if line.trim().is_empty() {
            format!("{symbol},")
        } else {
            format!(" {symbol},")
        };
        if line.len() + next.len() > 100 {
            output.push_str(&line);
            output.push('\n');
            line.clear();
            line.push_str("    ");
            line.push_str(symbol);
            line.push(',');
        } else {
            line.push_str(&next);
        }
    }
    if !line.trim().is_empty() {
        output.push_str(&line);
        output.push('\n');
    }
    output.push_str("};");
    output
}

fn detects_private_module_conflict(existing: &str, required_line: &str) -> bool {
    let Some(module_name) = required_line
        .strip_prefix("pub mod ")
        .and_then(|line| line.strip_suffix(';'))
    else {
        return false;
    };
    let private_line = format!("mod {module_name};");
    existing
        .lines()
        .any(|existing_line| existing_line.trim() == private_line)
}

fn validate_patch_identifier(value: &str, label: &str, path: &Path) -> Result<(), CodegenError> {
    if value.is_empty()
        || value.as_bytes()[0].is_ascii_digit()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return unsafe_patch(
            path,
            format!("{label} must be a Rust-style ASCII identifier"),
        );
    }
    Ok(())
}

fn validate_module_path(value: &str, label: &str, path: &Path) -> Result<(), CodegenError> {
    if value.is_empty() || value.contains(":::") {
        return unsafe_patch(path, format!("{label} must be a Rust module path"));
    }

    for segment in value.split("::") {
        validate_patch_identifier(segment, label, path)?;
    }

    Ok(())
}
