use leptos_ui_kit_registry::KitConfig;

use crate::path_safety::PlanningContext;
use crate::planning::push_file_plan;
use crate::{ChangeKind, ChangeRecord, CodegenError, PlannedFile, PlannedFileAction};

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
