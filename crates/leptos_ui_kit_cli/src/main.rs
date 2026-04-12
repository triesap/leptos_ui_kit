#![forbid(unsafe_code)]

use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process,
};

use leptos_ui_kit_registry::{InfoOutput, build_info_output};

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

fn usage() -> String {
    "usage: leptos_ui_kit_cli info [--json] [path]".to_owned()
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
}
