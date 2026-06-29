#![forbid(unsafe_code)]

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

use leptos_ui_kit_codegen::{
    AddPlan, CommandEnvelope, CommandStatus, Diagnostic, DiagnosticLevel, InitPlan, InstallState,
    InstalledFile, InstalledItem, InstalledStyleBlock, apply_add, apply_init,
    extract_managed_css_block, hash_content_bytes, parse_install_state_str, plan_add, plan_init,
};
use leptos_ui_kit_registry::{
    DependencyStatus, InfoOutput, ResolvedRegistryItem, build_info_output,
    load_built_in_registry_item, load_registry_item, read_built_in_registry_source,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InfoCommandOutput {
    #[serde(flatten)]
    info: InfoOutput,
    registry_available: bool,
    installed_state: Option<InstallState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryItemSourceOutput {
    resolved: ResolvedRegistryItem,
    sources: Vec<RegistrySourceContent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistrySourceContent {
    path: String,
    kind: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorOutput {
    project_root: PathBuf,
    strict: bool,
    check: bool,
    trunk_build: bool,
    checks: Vec<DoctorCheck>,
}

impl DoctorOutput {
    fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorCheckStatus::Fail)
    }

    fn has_warnings(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorCheckStatus::Warning)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheck {
    name: String,
    status: DoctorCheckStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

impl DoctorCheck {
    fn pass(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorCheckStatus::Pass,
            message: message.into(),
            path: None,
        }
    }

    fn warning(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorCheckStatus::Warning,
            message: message.into(),
            path: None,
        }
    }

    fn fail(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorCheckStatus::Fail,
            message: message.into(),
            path: None,
        }
    }

    fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorCheckStatus {
    Pass,
    Warning,
    Fail,
}

fn main() {
    if let Err(error) = run(env::args_os().skip(1).collect(), &current_dir()) {
        eprintln!("{error}");
        process::exit(exit_code_for_error(&error));
    }
}

fn exit_code_for_error(error: &str) -> i32 {
    if error == "doctor checks failed" {
        return 3;
    }
    if error.starts_with("usage:")
        || error.contains("unsupported flag")
        || error.contains("does not accept")
        || error.contains("requires")
        || error.contains("accepts exactly")
        || error.contains("accepts at most")
        || error.contains("non-utf8")
    {
        return 2;
    }
    if error.contains("local edits")
        || error.contains("not tracked")
        || error.contains("already tracked")
        || error.contains("already exists")
    {
        return 10;
    }
    if error.contains("unsafe write path") || error.contains("path escapes") {
        return 11;
    }
    if error.contains("built-in registry")
        || error.contains("registry")
        || error.contains("package")
        || error.contains("not found")
    {
        return 12;
    }
    1
}

fn run(args: Vec<OsString>, cwd: &Path) -> Result<(), String> {
    let (args, cwd, _quiet, _verbose) = parse_common_args(args, cwd)?;
    let Some(command) = args.first().and_then(|value| value.to_str()) else {
        return Err(usage());
    };

    match command {
        "add" => run_add(&args[1..], &cwd),
        "doctor" => run_doctor(&args[1..], &cwd),
        "info" => run_info(&args[1..], &cwd),
        "init" => run_init(&args[1..], &cwd),
        "view" => run_view(&args[1..], &cwd),
        _ => Err(usage()),
    }
}

fn parse_common_args(
    args: Vec<OsString>,
    cwd: &Path,
) -> Result<(Vec<OsString>, PathBuf, bool, bool), String> {
    let mut filtered = Vec::new();
    let mut target_cwd = cwd.to_path_buf();
    let mut quiet = false;
    let mut verbose = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--cwd") => {
                let Some(path) = iter.next() else {
                    return Err("--cwd requires a path".to_owned());
                };
                let path = PathBuf::from(path);
                target_cwd = if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                };
            }
            Some("--quiet") => quiet = true,
            Some("--verbose") => verbose = true,
            _ => filtered.push(arg),
        }
    }

    Ok((filtered, target_cwd, quiet, verbose))
}

fn run_add(args: &[OsString], cwd: &Path) -> Result<(), String> {
    let mut json = false;
    let mut dry_run = false;
    let mut item: Option<String> = None;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err("non-utf8 arguments are not supported".to_owned());
        };

        match value {
            "--json" => json = true,
            "--dry-run" => dry_run = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported flag for add: {value}"));
            }
            value => {
                if item.is_some() {
                    return Err("add accepts exactly one item name".to_owned());
                }

                item = Some(value.to_owned());
            }
        }
    }

    let item = item.ok_or_else(|| "add requires an item name".to_owned())?;
    let plan = if dry_run {
        plan_add(cwd, &item)
            .map_err(|error| format!("failed to plan add {item} for {}: {error}", cwd.display()))?
    } else {
        apply_add(cwd, &item)
            .map_err(|error| format!("failed to add {item} to {}: {error}", cwd.display()))?
    };
    let status = if dry_run {
        CommandStatus::Planned
    } else if plan.is_empty() {
        CommandStatus::NoChange
    } else {
        CommandStatus::Success
    };

    println!("{}", render_add_plan(&plan, json, status)?);

    Ok(())
}

fn run_doctor(args: &[OsString], cwd: &Path) -> Result<(), String> {
    let mut json = false;
    let mut strict = false;
    let mut check = false;
    let mut trunk_build = false;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err("non-utf8 arguments are not supported".to_owned());
        };

        match value {
            "--json" => json = true,
            "--strict" => strict = true,
            "--check" => check = true,
            "--trunk-build" => trunk_build = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported flag for doctor: {value}"));
            }
            _ => return Err("doctor does not accept positional arguments".to_owned()),
        }
    }

    let output = build_doctor_output(cwd, strict, check, trunk_build);
    let status = doctor_status(&output);
    println!("{}", render_doctor_output(&output, json, status)?);
    if output.has_failures() {
        return Err("doctor checks failed".to_owned());
    }

    Ok(())
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

    let plan = if dry_run {
        plan_init(cwd)
            .map_err(|error| format!("failed to plan init for {}: {error}", cwd.display()))?
    } else {
        apply_init(cwd)
            .map_err(|error| format!("failed to initialize {}: {error}", cwd.display()))?
    };

    let status = if dry_run {
        CommandStatus::Planned
    } else if plan.is_empty() {
        CommandStatus::NoChange
    } else {
        CommandStatus::Success
    };

    println!("{}", render_init_plan(&plan, json, status)?);

    Ok(())
}

fn run_view(args: &[OsString], cwd: &Path) -> Result<(), String> {
    let mut json = false;
    let mut include_source = false;
    let mut registry_source: Option<String> = None;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err("non-utf8 arguments are not supported".to_owned());
        };

        match value {
            "--json" => json = true,
            "--source" => include_source = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported flag for view: {value}"));
            }
            value => {
                if registry_source.is_some() {
                    return Err("view accepts exactly one registry source".to_owned());
                }

                registry_source = Some(value.to_owned());
            }
        }
    }

    let source = registry_source.ok_or_else(|| "view requires a registry source".to_owned())?;
    let item = load_registry_item(&source, cwd)
        .map_err(|error| format!("failed to load registry item {source}: {error}"))?;

    println!("{}", render_registry_item(&item, json, include_source)?);

    Ok(())
}

fn render_add_plan(plan: &AddPlan, json: bool, status: CommandStatus) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(
            &CommandEnvelope::new("add", status, plan)
                .with_changes(plan.changes.clone())
                .with_diagnostics(plan.diagnostics.clone()),
        )
        .map_err(|error| format!("failed to serialize add plan: {error}"));
    }

    if plan.is_empty() {
        return Ok(format!("add {}: no changes planned", plan.item_name));
    }

    let mut output = format!("add {} planned changes:", plan.item_name);
    for change in &plan.changes {
        output.push_str(&format!("\n- {:?} {}", change.kind, change.path));
    }
    Ok(output)
}

fn render_init_plan(plan: &InitPlan, json: bool, status: CommandStatus) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(
            &CommandEnvelope::new("init", status, plan).with_changes(plan.changes.clone()),
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
    let command_output = InfoCommandOutput {
        info: output.clone(),
        registry_available: load_built_in_registry_item("button").is_ok(),
        installed_state: read_installed_state(&output.detected.project_root),
    };

    if json {
        return serde_json::to_string_pretty(&CommandEnvelope::success("info", &command_output))
            .map_err(|error| format!("failed to serialize info output: {error}"));
    }

    Ok(format!(
        "project_root: {}\nworkspace_mode: {:?}\nsource_root: {}\nindex_html: {}\ncss_file: {}\nrender_mode: {}\nregistry_available: {}\ninstalled_state: {}",
        output.detected.project_root.display(),
        output.detected.workspace_mode,
        output.detected.source_root.display(),
        output.detected.index_html_path.display(),
        output.detected.css_file_path.display(),
        output
            .detected
            .render_mode
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "unknown".to_owned()),
        command_output.registry_available,
        command_output.installed_state.is_some()
    ))
}

fn render_registry_item(
    item: &ResolvedRegistryItem,
    json: bool,
    include_source: bool,
) -> Result<String, String> {
    if include_source {
        let output = registry_item_source_output(item)?;
        if json {
            return serde_json::to_string_pretty(&CommandEnvelope::success("view", output))
                .map_err(|error| format!("failed to serialize registry item source: {error}"));
        }

        let mut rendered = format!(
            "name: {}\nkind: {}\ncontent_hash: {}",
            output.resolved.item.name, output.resolved.item.kind, output.resolved.content_hash
        );
        for source in output.sources {
            rendered.push_str(&format!(
                "\n--- {} ({}) ---\n{}",
                source.path, source.kind, source.content
            ));
        }
        return Ok(rendered);
    }

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

fn registry_item_source_output(
    item: &ResolvedRegistryItem,
) -> Result<RegistryItemSourceOutput, String> {
    let mut sources = Vec::new();
    for file in &item.targets.ui_files {
        sources.push(RegistrySourceContent {
            path: file.source.clone(),
            kind: "rust".to_owned(),
            content: read_built_in_registry_source(&file.source)
                .map_err(|error| format!("failed to read {}: {error}", file.source))?,
        });
    }
    for style in &item.targets.style_blocks {
        sources.push(RegistrySourceContent {
            path: style.source.clone(),
            kind: "css".to_owned(),
            content: read_built_in_registry_source(&style.source)
                .map_err(|error| format!("failed to read {}: {error}", style.source))?,
        });
    }

    Ok(RegistryItemSourceOutput {
        resolved: item.clone(),
        sources,
    })
}

fn build_doctor_output(cwd: &Path, strict: bool, check: bool, trunk_build: bool) -> DoctorOutput {
    let mut checks = Vec::new();

    match build_info_output(cwd) {
        Ok(info) => {
            checks.push(DoctorCheck::pass(
                "project",
                "supported Trunk CSR project detected",
            ));
            if info.components_config.is_some() {
                checks.push(DoctorCheck::pass("config", "components.json is valid"));
            } else {
                checks.push(strict_check(
                    strict,
                    "config",
                    "components.json is missing; run leptos_ui_kit init",
                ));
            }

            dependency_check(
                &mut checks,
                strict,
                "dependency.leptos",
                "leptos",
                info.detected.dependency_plan.leptos.status,
            );
            dependency_check(
                &mut checks,
                strict,
                "dependency.leptos_router",
                "leptos_router",
                info.detected.dependency_plan.leptos_router.status,
            );

            checks.extend(state_checks(cwd, strict));
        }
        Err(error) => {
            checks.push(DoctorCheck::fail("project", error.to_string()));
        }
    }

    match load_built_in_registry_item("button") {
        Ok(item) => {
            let mut registry_ok = true;
            for file in &item.targets.ui_files {
                registry_ok &= read_built_in_registry_source(&file.source).is_ok();
            }
            for style in &item.targets.style_blocks {
                registry_ok &= read_built_in_registry_source(&style.source).is_ok();
            }
            if registry_ok {
                checks.push(DoctorCheck::pass(
                    "registry",
                    "built-in registry button assets are available",
                ));
            } else {
                checks.push(DoctorCheck::fail(
                    "registry",
                    "built-in registry button assets are incomplete",
                ));
            }
        }
        Err(error) => {
            checks.push(DoctorCheck::fail("registry", error.to_string()));
        }
    }

    if check {
        checks.push(run_command_check(
            "build.cargo_check",
            cwd,
            "cargo",
            &["check", "--target", "wasm32-unknown-unknown"],
        ));
    }

    if trunk_build {
        checks.push(run_command_check("build.trunk", cwd, "trunk", &["build"]));
    }

    DoctorOutput {
        project_root: cwd.to_path_buf(),
        strict,
        check,
        trunk_build,
        checks,
    }
}

fn run_command_check(name: &str, cwd: &Path, program: &str, args: &[&str]) -> DoctorCheck {
    match Command::new(program).args(args).current_dir(cwd).output() {
        Ok(output) if output.status.success() => {
            DoctorCheck::pass(name, format!("{} {} passed", program, args.join(" ")))
        }
        Ok(output) => DoctorCheck::fail(
            name,
            format!(
                "{} {} failed: {}",
                program,
                args.join(" "),
                command_output_summary(&output.stdout, &output.stderr)
            ),
        ),
        Err(error) => DoctorCheck::fail(
            name,
            format!("failed to run {} {}: {error}", program, args.join(" ")),
        ),
    }
}

fn command_output_summary(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let summary = if !stderr.is_empty() { stderr } else { stdout };
    if summary.is_empty() {
        "process exited with a non-zero status".to_owned()
    } else {
        summary.chars().take(600).collect()
    }
}

fn dependency_check(
    checks: &mut Vec<DoctorCheck>,
    strict: bool,
    name: &str,
    crate_name: &str,
    status: DependencyStatus,
) {
    match status {
        DependencyStatus::Satisfied => {
            checks.push(DoctorCheck::pass(
                name,
                format!("{crate_name} dependency is satisfied"),
            ));
        }
        DependencyStatus::Missing => checks.push(strict_check(
            strict,
            name,
            format!("{crate_name} dependency is missing"),
        )),
        DependencyStatus::Incompatible => checks.push(strict_check(
            strict,
            name,
            format!("{crate_name} dependency is incompatible"),
        )),
    }
}

fn strict_check(strict: bool, name: impl Into<String>, message: impl Into<String>) -> DoctorCheck {
    if strict {
        DoctorCheck::fail(name, message)
    } else {
        DoctorCheck::warning(name, message)
    }
}

fn state_checks(cwd: &Path, strict: bool) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let state_path = cwd.join(".leptos-ui/state.json");
    if !state_path.is_file() {
        checks.push(strict_check(
            strict,
            "state",
            ".leptos-ui/state.json is missing",
        ));
        return checks;
    }

    let state_input = match fs::read_to_string(&state_path) {
        Ok(input) => input,
        Err(error) => {
            checks.push(
                DoctorCheck::fail("state", format!("failed to read state: {error}"))
                    .with_path(state_path.display().to_string()),
            );
            return checks;
        }
    };
    let state = match parse_install_state_str(&state_input) {
        Ok(state) => state,
        Err(error) => {
            checks.push(
                DoctorCheck::fail("state", error.to_string())
                    .with_path(state_path.display().to_string()),
            );
            return checks;
        }
    };

    checks.push(DoctorCheck::pass("state", "install state is valid"));
    for item in state.items.values() {
        checks.push(compare_item_content_hash(item));
        for file in &item.files {
            let source_path = cwd.join(&file.path);
            let baseline_path = cwd.join(&file.baseline_path);
            checks.extend(compare_file_to_baseline(
                "installed_file",
                file,
                &source_path,
                &baseline_path,
                strict,
            ));
        }
        for block in &item.style_blocks {
            let css_path = cwd.join(&block.css_path);
            let baseline_path = cwd.join(&block.baseline_path);
            checks.extend(compare_css_block_to_baseline(
                block,
                &css_path,
                &block.block_id,
                &baseline_path,
                strict,
            ));
        }
    }

    checks
}

fn compare_item_content_hash(item: &InstalledItem) -> DoctorCheck {
    match load_built_in_registry_item(&item.name) {
        Ok(registry_item) if item.content_hash == registry_item.content_hash => {
            DoctorCheck::pass("item_content_hash", "item content hash matches registry")
        }
        Ok(_) => DoctorCheck::fail(
            "item_content_hash",
            format!("item {} content hash differs from registry", item.id),
        ),
        Err(error) => DoctorCheck::fail("item_content_hash", error.to_string()),
    }
}

fn compare_file_to_baseline(
    name: &str,
    file: &InstalledFile,
    source_path: &Path,
    baseline_path: &Path,
    strict: bool,
) -> Vec<DoctorCheck> {
    let source = match fs::read_to_string(source_path) {
        Ok(source) => source,
        Err(error) => {
            return vec![
                DoctorCheck::fail(name, format!("failed to read installed file: {error}"))
                    .with_path(source_path.display().to_string()),
            ];
        }
    };
    let baseline = match fs::read_to_string(baseline_path) {
        Ok(baseline) => baseline,
        Err(error) => {
            return vec![
                DoctorCheck::fail(name, format!("failed to read baseline: {error}"))
                    .with_path(baseline_path.display().to_string()),
            ];
        }
    };
    let source_hash = hash_content_bytes(source.as_bytes());
    let baseline_hash = hash_content_bytes(baseline.as_bytes());
    let mut checks = Vec::new();

    if baseline_hash == file.baseline_hash {
        checks.push(
            DoctorCheck::pass("baseline_hash", "file baseline hash matches state")
                .with_path(baseline_path.display().to_string()),
        );
    } else {
        checks.push(
            DoctorCheck::fail("baseline_hash", "file baseline hash differs from state")
                .with_path(baseline_path.display().to_string()),
        );
    }

    if source_hash == file.local_hash_at_install {
        checks.push(
            DoctorCheck::pass(
                "installed_file_hash",
                "installed file hash matches install state",
            )
            .with_path(source_path.display().to_string()),
        );
    } else {
        checks.push(
            strict_check(
                strict,
                "installed_file_hash",
                "installed file hash differs from install state",
            )
            .with_path(source_path.display().to_string()),
        );
    }

    checks.push(if source == baseline {
        DoctorCheck::pass(name, "installed file matches baseline")
            .with_path(source_path.display().to_string())
    } else {
        strict_check(strict, name, "installed file differs from baseline")
            .with_path(source_path.display().to_string())
    });

    checks
}

fn compare_css_block_to_baseline(
    block: &InstalledStyleBlock,
    css_path: &Path,
    block_id: &str,
    baseline_path: &Path,
    strict: bool,
) -> Vec<DoctorCheck> {
    let css = match fs::read_to_string(css_path) {
        Ok(css) => css,
        Err(error) => {
            return vec![
                DoctorCheck::fail("style_block", format!("failed to read CSS: {error}"))
                    .with_path(css_path.display().to_string()),
            ];
        }
    };
    let baseline = match fs::read_to_string(baseline_path) {
        Ok(baseline) => baseline,
        Err(error) => {
            return vec![
                DoctorCheck::fail(
                    "style_block",
                    format!("failed to read CSS baseline: {error}"),
                )
                .with_path(baseline_path.display().to_string()),
            ];
        }
    };
    let baseline_hash = hash_content_bytes(baseline.as_bytes());
    let mut checks = Vec::new();

    if baseline_hash == block.baseline_hash {
        checks.push(
            DoctorCheck::pass("style_block_hash", "CSS baseline hash matches state")
                .with_path(baseline_path.display().to_string()),
        );
    } else {
        checks.push(
            DoctorCheck::fail("style_block_hash", "CSS baseline hash differs from state")
                .with_path(baseline_path.display().to_string()),
        );
    }

    match extract_managed_css_block(&css, block_id) {
        Ok(Some(current)) if current == baseline => checks.push(
            DoctorCheck::pass(
                "style_block",
                format!("managed CSS block {block_id} matches baseline"),
            )
            .with_path(css_path.display().to_string()),
        ),
        Ok(Some(_)) => checks.push(
            strict_check(
                strict,
                "style_block",
                format!("managed CSS block {block_id} differs from baseline"),
            )
            .with_path(css_path.display().to_string()),
        ),
        Ok(None) => checks.push(
            strict_check(
                strict,
                "style_block",
                format!("managed CSS block {block_id} is missing"),
            )
            .with_path(css_path.display().to_string()),
        ),
        Err(error) => checks.push(
            DoctorCheck::fail("style_block", error.to_string())
                .with_path(css_path.display().to_string()),
        ),
    }

    checks
}

fn doctor_status(output: &DoctorOutput) -> CommandStatus {
    if output.has_failures() {
        CommandStatus::Error
    } else if output.has_warnings() {
        CommandStatus::Warning
    } else {
        CommandStatus::Success
    }
}

fn render_doctor_output(
    output: &DoctorOutput,
    json: bool,
    status: CommandStatus,
) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(
            &CommandEnvelope::new("doctor", status, output)
                .with_diagnostics(doctor_diagnostics(output)),
        )
        .map_err(|error| format!("failed to serialize doctor output: {error}"));
    }

    let mut rendered = String::from("doctor checks:");
    for check in &output.checks {
        rendered.push_str(&format!(
            "\n- {:?} {}: {}",
            check.status, check.name, check.message
        ));
    }
    Ok(rendered)
}

fn doctor_diagnostics(output: &DoctorOutput) -> Vec<Diagnostic> {
    output
        .checks
        .iter()
        .filter_map(|check| match check.status {
            DoctorCheckStatus::Pass => None,
            DoctorCheckStatus::Warning => Some(Diagnostic::new(
                DiagnosticLevel::Warning,
                format!("doctor.{}", check.name),
                check.message.clone(),
            )),
            DoctorCheckStatus::Fail => Some(Diagnostic::new(
                DiagnosticLevel::Error,
                format!("doctor.{}", check.name),
                check.message.clone(),
            )),
        })
        .map(|diagnostic| {
            if let Some(path) = output
                .checks
                .iter()
                .find(|check| format!("doctor.{}", check.name) == diagnostic.code)
                .and_then(|check| check.path.clone())
            {
                diagnostic.with_path(path)
            } else {
                diagnostic
            }
        })
        .collect()
}

fn read_installed_state(project_root: &Path) -> Option<InstallState> {
    let path = project_root.join(".leptos-ui/state.json");
    let input = fs::read_to_string(path).ok()?;
    parse_install_state_str(&input).ok()
}

fn usage() -> String {
    "usage: leptos_ui_kit <add|doctor|info|init|view> [--json] [--dry-run] [path-or-source]"
        .to_owned()
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
        let output = render_registry_item(&item, true, false).expect("render json");

        assert!(output.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(output.contains("\"command\": \"view\""));
        assert!(output.contains("\"name\": \"button\""));
        assert!(output.contains("\"source_kind\": \"built-in\""));
        assert!(output.contains("\"kind\": \"ui\""));
    }

    #[test]
    fn view_source_outputs_registry_source_contents() {
        let item = load_registry_item("button", Path::new(".")).expect("load built-in item");
        let output = render_registry_item(&item, true, true).expect("render json");

        assert!(output.contains("\"sources\""));
        assert!(output.contains("pub fn Button"));
        assert!(output.contains(".luk-button"));
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

        let output = render_init_plan(
            &plan_init(root).expect("plan init"),
            true,
            CommandStatus::Planned,
        )
        .expect("render");
        assert!(output.contains("\"command\": \"init\""));
        assert!(output.contains("\"status\": \"planned\""));
        assert!(output.contains("\"path\": \"components.json\""));
        assert!(!root.join("components.json").exists());
    }

    #[test]
    fn init_write_creates_files() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");

        run(vec![OsString::from("init")], root).expect("run init");

        assert!(root.join("components.json").is_file());
        assert!(root.join(".leptos-ui/state.json").is_file());
    }

    #[test]
    fn add_dry_run_envelope_json_outputs_planned_changes_without_writes() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");

        run(
            vec![
                OsString::from("add"),
                OsString::from("button"),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run add dry-run");

        let output = render_add_plan(
            &plan_add(root, "button").expect("plan add"),
            true,
            CommandStatus::Planned,
        )
        .expect("render add");
        assert!(output.contains("\"command\": \"add\""));
        assert!(output.contains("\"status\": \"planned\""));
        assert!(output.contains("\"itemName\": \"button\""));
        assert!(output.contains("\"path\": \"src/components/ui/button.rs\""));
        assert!(output.contains("\"path\": \".leptos-ui/baselines/builtin-button/button.rs\""));
        assert!(!root.join("src/components/ui/button.rs").exists());
    }

    #[test]
    fn add_write_installs_button_and_then_reports_no_change() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");

        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");
        assert!(root.join("src/components/ui/button.rs").is_file());
        assert!(
            root.join(".leptos-ui/baselines/builtin-button/button.css")
                .is_file()
        );

        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run second add");
        let output = render_add_plan(
            &plan_add(root, "button").expect("plan add"),
            true,
            CommandStatus::NoChange,
        )
        .expect("render add");
        assert!(output.contains("\"status\": \"no_change\""));
    }

    #[test]
    fn doctor_strict_json_passes_after_init_and_add() {
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
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        run(
            vec![
                OsString::from("doctor"),
                OsString::from("--strict"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run doctor");
        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert!(output.contains("\"command\": \"doctor\""));
        assert!(output.contains("\"status\": \"success\""));
        assert!(output.contains("\"name\": \"registry\""));
        assert!(output.contains("\"status\": \"pass\""));
    }

    #[test]
    fn doctor_reports_state_hash_mismatches() {
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
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        let state_path = root.join(".leptos-ui/state.json");
        let mut state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        state["items"]["builtin:button"]["files"][0]["baselineHash"] =
            serde_json::Value::String(format!("sha256:{}", "0".repeat(64)));
        fs::write(
            &state_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&state).expect("serialize state")
            ),
        )
        .expect("write state");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.baseline_hash\""));
        assert!(output.contains("file baseline hash differs from state"));
    }

    #[test]
    fn doctor_rejects_duplicate_managed_css_blocks() {
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
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        let css_path = root.join("styles/app.css");
        let baseline =
            fs::read_to_string(root.join(".leptos-ui/baselines/builtin-button/button.css"))
                .expect("read baseline");
        let mut css = fs::read_to_string(&css_path).expect("read css");
        css.push('\n');
        css.push_str(&baseline);
        fs::write(&css_path, css).expect("write css");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.style_block\""));
        assert!(output.contains("managed CSS block button markers are ambiguous"));
    }

    #[test]
    fn unsupported_flags_return_usage_errors() {
        let error = run(
            vec![
                OsString::from("view"),
                OsString::from("button"),
                OsString::from("--tailwind"),
            ],
            Path::new("."),
        )
        .expect_err("tailwind flag should be unsupported");

        assert!(error.contains("unsupported flag for view"));
    }

    #[test]
    fn common_flags_are_accepted_before_dispatch() {
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
        .expect("write index");

        run(
            vec![
                OsString::from("--cwd"),
                root.as_os_str().to_owned(),
                OsString::from("--quiet"),
                OsString::from("--verbose"),
                OsString::from("info"),
                OsString::from("--json"),
            ],
            Path::new("."),
        )
        .expect("run info with common flags");
    }

    #[test]
    fn exit_code_mapping_matches_contract() {
        assert_eq!(
            exit_code_for_error("unsupported flag for view: --tailwind"),
            2
        );
        assert_eq!(exit_code_for_error("doctor checks failed"), 3);
        assert_eq!(
            exit_code_for_error(
                "cannot safely patch src/components/ui/button.rs: target exists but is not tracked in state"
            ),
            10
        );
        assert_eq!(
            exit_code_for_error("unsafe write path ../evil.rs: parent traversal"),
            11
        );
        assert_eq!(
            exit_code_for_error("built-in registry item not found: nope"),
            12
        );
        assert_eq!(exit_code_for_error("failed to inspect project"), 1);
    }

    #[test]
    fn doctor_strict_failure_returns_doctor_error() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();

        let error = run(
            vec![
                OsString::from("doctor"),
                OsString::from("--strict"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect_err("doctor should fail");

        assert_eq!(error, "doctor checks failed");
        assert_eq!(exit_code_for_error(&error), 3);
    }

    #[test]
    fn doctor_command_check_reports_missing_tools() {
        let dir = tempdir().expect("tempdir");
        let check = run_command_check(
            "build.fake",
            dir.path(),
            "leptos_ui_kit_definitely_missing_tool",
            &["build"],
        );

        assert_eq!(check.status, DoctorCheckStatus::Fail);
        assert!(check.message.contains("failed to run"));
    }
}
