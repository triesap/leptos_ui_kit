#![forbid(unsafe_code)]

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process,
};

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

fn render_info_output(output: &InfoOutput, json: bool) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(output)
            .map_err(|error| format!("failed to serialize info output: {error}"));
    }

    Ok(format!(
        "project_root: {}\nworkspace_mode: {:?}\nsource_root: {}\nrender_mode: {}\ntailwind_css: {}",
        output.detected.project_root.display(),
        output.detected.workspace_mode,
        output.detected.source_root.display(),
        output
            .detected
            .render_mode
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "unknown".to_owned()),
        output
            .detected
            .tailwind
            .css_entry
            .as_ref()
            .map(|value| value.display().to_string())
            .unwrap_or_else(|| "none".to_owned())
    ))
}

fn render_registry_item(item: &ResolvedRegistryItem, json: bool) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(item)
            .map_err(|error| format!("failed to serialize registry item: {error}"));
    }

    Ok(format!(
        "name: {}\ntype: {}\nsource_kind: {:?}\nsource_path: {}",
        item.item.name,
        item.item.kind,
        item.source_kind,
        item.source_path.display()
    ))
}

fn usage() -> String {
    "usage: leptos-ui <info|view> [--json] [path-or-source]".to_owned()
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
    fn info_json_outputs_detected_project_shape() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2024"

[dependencies]
leptos = { version = "0.8.17", features = ["csr"] }
"#,
        )
        .expect("write cargo");
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("Trunk.toml"),
            "[tools]\ntailwindcss = \"4.1.13\"\n",
        )
        .expect("write trunk");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="tailwind-css" href="input.css" />
  </head>
  <body></body>
</html>
"#,
        )
        .expect("write html");
        fs::write(root.join("input.css"), "@import \"tailwindcss\";\n").expect("write css");

        let info = build_info_output(root).expect("build info output");
        let output = render_info_output(&info, true).expect("render json");

        assert!(output.contains("\"project_root\""));
        assert!(output.contains("\"render_mode\": \"csr\""));
        assert!(output.contains("\"css_entry\""));
    }

    #[test]
    fn view_json_outputs_built_in_registry_item() {
        let item = load_registry_item("button", Path::new(".")).expect("load built-in item");
        let output = render_registry_item(&item, true).expect("render json");

        assert!(output.contains("\"name\": \"button\""));
        assert!(output.contains("\"source_kind\": \"built-in\""));
        assert!(output.contains("\"type\": \"registry:ui\""));
    }
}
