#![forbid(unsafe_code)]

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process,
};

use leptos_ui_kit_codegen::{CommandEnvelope, CommandStatus, InitPlan, plan_init};
use leptos_ui_kit_registry::{
    InfoOutput, ResolvedRegistryItem, build_info_output, load_registry_item,
};

fn main() {
    if let Err(error) = run(env::args_os().skip(1).collect(), &current_dir()) {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn run(args: Vec<OsString>, cwd: &Path) -> Result<(), String> {
    let Some(command) = args.first().and_then(|value| value.to_str()) else {
        return Err(usage());
    };

    match command {
        "info" => run_info(&args[1..], cwd),
        "init" => run_init(&args[1..], cwd),
        "view" => run_view(&args[1..], cwd),
        _ => Err(usage()),
    }
}

fn run_info(args: &[OsString], cwd: &Path) -> Result<(), String> {
    let mut json = false;
    let mut path: Option<PathBuf> = None;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err("non-utf8 arguments are not supported".to_owned());
        };

        match value {
            "--json" => json = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported flag for info: {value}"));
            }
            value => {
                if path.is_some() {
                    return Err("info accepts at most one path argument".to_owned());
                }

                path = Some(PathBuf::from(value));
            }
        }
    }

    let target = path.unwrap_or_else(|| cwd.to_path_buf());
    let output = build_info_output(&target)
        .map_err(|error| format!("failed to inspect {}: {error}", target.display()))?;

    println!("{}", render_info_output(&output, json)?);

    Ok(())
}

fn run_init(args: &[OsString], cwd: &Path) -> Result<(), String> {
    let mut json = false;
    let mut dry_run = false;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err("non-utf8 arguments are not supported".to_owned());
        };

        match value {
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported flag for init: {value}"));
            }
            _ => return Err("init does not accept positional arguments".to_owned()),
        }
    }

    if !dry_run {
        return Err("init write mode is not implemented yet; use --dry-run".to_owned());
    }

    let plan = plan_init(cwd)
        .map_err(|error| format!("failed to plan init for {}: {error}", cwd.display()))?;

    println!("{}", render_init_plan(&plan, json)?);

    Ok(())
}

fn run_view(args: &[OsString], cwd: &Path) -> Result<(), String> {
    let mut json = false;
    let mut source: Option<String> = None;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err("non-utf8 arguments are not supported".to_owned());
        };

        match value {
            "--json" => json = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported flag for view: {value}"));
            }
            value => {
                if source.is_some() {
                    return Err("view accepts exactly one registry source".to_owned());
                }

                source = Some(value.to_owned());
            }
        }
    }

    let source = source.ok_or_else(|| "view requires a registry source".to_owned())?;
    let item = load_registry_item(&source, cwd)
        .map_err(|error| format!("failed to load registry item {source}: {error}"))?;

    println!("{}", render_registry_item(&item, json)?);

    Ok(())
}

fn render_init_plan(plan: &InitPlan, json: bool) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(
            &CommandEnvelope::new("init", CommandStatus::Planned, plan)
                .with_changes(plan.changes.clone()),
        )
        .map_err(|error| format!("failed to serialize init plan: {error}"));
    }

    if plan.is_empty() {
        return Ok("init: no changes planned".to_owned());
    }

    let mut output = String::from("init planned changes:");
    for change in &plan.changes {
        output.push_str(&format!("\n- {:?} {}", change.kind, change.path));
    }
    Ok(output)
}

fn render_info_output(output: &InfoOutput, json: bool) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(&CommandEnvelope::success("info", output))
            .map_err(|error| format!("failed to serialize info output: {error}"));
    }

    Ok(format!(
        "project_root: {}\nworkspace_mode: {:?}\nsource_root: {}\nindex_html: {}\ncss_file: {}\nrender_mode: {}",
        output.detected.project_root.display(),
        output.detected.workspace_mode,
        output.detected.source_root.display(),
        output.detected.index_html_path.display(),
        output.detected.css_file_path.display(),
        output
            .detected
            .render_mode
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "unknown".to_owned())
    ))
}

fn render_registry_item(item: &ResolvedRegistryItem, json: bool) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(&CommandEnvelope::success("view", item))
            .map_err(|error| format!("failed to serialize registry item: {error}"));
    }

    Ok(format!(
        "name: {}\nkind: {}\nsource_kind: {:?}\nsource_path: {}",
        item.item.name,
        item.item.kind,
        item.source_kind,
        item.source_path.display()
    ))
}

fn usage() -> String {
    "usage: leptos-ui <info|init|view> [--json] [--dry-run] [path-or-source]".to_owned()
}

fn current_dir() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::tempdir;

    #[test]
    fn info_envelope_json_outputs_detected_project_shape() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
leptos_router = "0.9.0-alpha"
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::create_dir(root.join("styles")).expect("create styles");
        fs::write(root.join("styles/app.css"), ":root {}\n").expect("write css");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="css" href="styles/app.css" />
  </head>
  <body></body>
</html>
"#,
        )
        .expect("write html");

        let info = build_info_output(root).expect("build info output");
        let output = render_info_output(&info, true).expect("render json");

        assert!(output.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(output.contains("\"command\": \"info\""));
        assert!(output.contains("\"project_root\""));
        assert!(output.contains("\"render_mode\": \"csr\""));
        assert!(output.contains("\"css_file_path\""));
    }

    #[test]
    fn view_envelope_json_outputs_built_in_registry_item() {
        let item = load_registry_item("button", Path::new(".")).expect("load built-in item");
        let output = render_registry_item(&item, true).expect("render json");

        assert!(output.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(output.contains("\"command\": \"view\""));
        assert!(output.contains("\"name\": \"button\""));
        assert!(output.contains("\"source_kind\": \"built-in\""));
        assert!(output.contains("\"kind\": \"ui\""));
    }

    #[test]
    fn init_dry_run_envelope_json_outputs_planned_changes() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        run(
            vec![
                OsString::from("init"),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run init dry-run");

        let output = render_init_plan(&plan_init(root).expect("plan init"), true).expect("render");
        assert!(output.contains("\"command\": \"init\""));
        assert!(output.contains("\"status\": \"planned\""));
        assert!(output.contains("\"path\": \"components.json\""));
        assert!(!root.join("components.json").exists());
    }
}
