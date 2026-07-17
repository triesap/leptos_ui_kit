#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{self, Command},
};

use leptos_ui_kit_codegen::{
    AddPlan, CommandEnvelope, CommandStatus, DEFAULT_KIT_LOCK_PATH, Diagnostic, DiagnosticLevel,
    InitPlan, InstallLock, InstalledFile, InstalledItem, InstalledStyleBlock, SyncPlan, apply_add,
    apply_init, apply_sync, extract_managed_css_block, hash_content_bytes, install_lock_path,
    parse_install_lock_str_at_path, plan_add, plan_init, plan_sync,
};
use leptos_ui_kit_registry::{
    CargoPlanEntry, DEFAULT_CSS_PATH, DEFAULT_KIT_CONFIG_PATH, DependencyRequirement,
    DependencyStatus, InfoOutput, KitConfig, ResolvedRegistryItem, SCHEMA_VERSION, TOOL_BINARY,
    TOOL_GIT_URL, TOOL_PACKAGE, ToolSourceConfig, build_info_output, canonical_tool_config,
    detect_cargo_plan_requirements, load_built_in_registry_item, load_built_in_registry_root,
    load_registry_item, read_built_in_registry_source,
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InfoCommandOutput {
    #[serde(flatten)]
    info: InfoOutput,
    registry_available: bool,
    installed_lock: Option<InstallLock>,
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
struct VersionCommandOutput {
    package: &'static str,
    binary: &'static str,
    version: &'static str,
    schema_version: &'static str,
    source: VersionSourceOutput,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionSourceOutput {
    kind: &'static str,
    url: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rev: Option<String>,
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

pub fn main_entry() {
    let args = normalize_args(env::args_os().skip(1).collect());
    if let Err(error) = run(args, &current_dir()) {
        eprintln!("{error}");
        process::exit(exit_code_for_error(&error));
    }
}

fn normalize_args(mut args: Vec<OsString>) -> Vec<OsString> {
    if args
        .first()
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| arg == "leptos_ui_kit")
    {
        args.remove(0);
    }

    args
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

    if command == "--help" || command == "-h" {
        println!("{}", help_text());
        return Ok(());
    }
    if command == "--version" || command == "-V" {
        return run_version(&args[1..]);
    }
    if args[1..].iter().any(|arg| is_help_arg(arg)) {
        println!("{}", command_help(command)?);
        return Ok(());
    }

    match command {
        "add" => run_add(&args[1..], &cwd),
        "doctor" => run_doctor(&args[1..], &cwd),
        "info" => run_info(&args[1..], &cwd),
        "init" => run_init(&args[1..], &cwd),
        "sync" => run_sync(&args[1..], &cwd),
        "view" => run_view(&args[1..], &cwd),
        _ => Err(usage()),
    }
}

fn is_help_arg(arg: &OsString) -> bool {
    arg.to_str()
        .is_some_and(|value| value == "--help" || value == "-h")
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

fn run_version(args: &[OsString]) -> Result<(), String> {
    let mut json = false;

    for arg in args {
        let Some(value) = arg.to_str() else {
            return Err("non-utf8 arguments are not supported".to_owned());
        };

        match value {
            "--json" => json = true,
            value if value.starts_with('-') => {
                return Err(format!("unsupported flag for version: {value}"));
            }
            _ => return Err("version does not accept positional arguments".to_owned()),
        }
    }

    println!("{}", render_version_output(json)?);
    Ok(())
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

fn run_sync(args: &[OsString], cwd: &Path) -> Result<(), String> {
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
                return Err(format!("unsupported flag for sync: {value}"));
            }
            _ => return Err("sync does not accept positional arguments".to_owned()),
        }
    }

    let plan = if dry_run {
        plan_sync(cwd)
            .map_err(|error| format!("failed to plan sync for {}: {error}", cwd.display()))?
    } else {
        apply_sync(cwd).map_err(|error| format!("failed to sync {}: {error}", cwd.display()))?
    };
    let status = if dry_run {
        CommandStatus::Planned
    } else if plan.is_empty() {
        CommandStatus::NoChange
    } else {
        CommandStatus::Success
    };

    println!("{}", render_sync_plan(&plan, json, status)?);

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
    append_cargo_plan_text(&mut output, &plan.cargo_plan);
    Ok(output)
}

fn render_sync_plan(plan: &SyncPlan, json: bool, status: CommandStatus) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(
            &CommandEnvelope::new("sync", status, plan)
                .with_changes(plan.changes.clone())
                .with_diagnostics(plan.diagnostics.clone()),
        )
        .map_err(|error| format!("failed to serialize sync plan: {error}"));
    }

    if plan.is_empty() {
        return Ok("sync: no changes planned".to_owned());
    }

    let mut output = "sync planned changes:".to_owned();
    for change in &plan.changes {
        output.push_str(&format!("\n- {:?} {}", change.kind, change.path));
    }
    append_cargo_plan_text(&mut output, &plan.cargo_plan);
    Ok(output)
}

fn append_cargo_plan_text(output: &mut String, cargo_plan: &[CargoPlanEntry]) {
    if cargo_plan.is_empty() {
        return;
    }

    output.push_str("\nrequired cargo dependencies:");
    for entry in cargo_plan {
        output.push_str(&format!("\n- {}", cargo_plan_entry_label(entry)));
    }
}

fn cargo_plan_entry_label(entry: &CargoPlanEntry) -> String {
    let source = match entry.source.kind {
        leptos_ui_kit_registry::CargoPlanSourceKind::Version => entry
            .source
            .version
            .as_deref()
            .map(|version| format!("version {version}"))
            .unwrap_or_else(|| "version <missing>".to_owned()),
        leptos_ui_kit_registry::CargoPlanSourceKind::Git => {
            let url = entry.source.url.as_deref().unwrap_or("<missing-url>");
            let rev = entry.source.rev.as_deref().unwrap_or("<missing-rev>");
            format!("git {url} rev {rev}")
        }
    };
    let features = if entry.features.is_empty() {
        String::new()
    } else {
        format!(" features [{}]", entry.features.join(", "))
    };

    format!("{} ({source}){features}", entry.crate_name)
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

fn render_version_output(json: bool) -> Result<String, String> {
    let output = version_output();

    if json {
        return serde_json::to_string_pretty(&CommandEnvelope::success("version", output))
            .map_err(|error| format!("failed to serialize version output: {error}"));
    }

    Ok(format!("{} {}", output.binary, output.version))
}

fn version_output() -> VersionCommandOutput {
    let rev = canonical_tool_config().ok().map(|tool| match tool.source {
        ToolSourceConfig::Git { rev, .. } => rev,
    });

    VersionCommandOutput {
        package: TOOL_PACKAGE,
        binary: TOOL_BINARY,
        version: env!("CARGO_PKG_VERSION"),
        schema_version: SCHEMA_VERSION,
        source: VersionSourceOutput {
            kind: "git",
            url: TOOL_GIT_URL,
            rev,
        },
    }
}

fn render_info_output(output: &InfoOutput, json: bool) -> Result<String, String> {
    let command_output = InfoCommandOutput {
        info: output.clone(),
        registry_available: validate_built_in_registry_assets().is_ok(),
        installed_lock: read_installed_lock(
            &output.detected.project_root,
            output.kit_config.as_ref(),
        ),
    };

    if json {
        return serde_json::to_string_pretty(&CommandEnvelope::success("info", &command_output))
            .map_err(|error| format!("failed to serialize info output: {error}"));
    }

    Ok(format!(
        "project_root: {}\nworkspace_mode: {:?}\nsource_root: {}\nindex_html: {}\ncss_file: {}\nrender_mode: {}\nregistry_available: {}\ninstalled_lock: {}",
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
        command_output.installed_lock.is_some()
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

fn validate_built_in_registry_assets() -> Result<(), String> {
    let root = load_built_in_registry_root().map_err(|error| error.to_string())?;

    for entry in root.items {
        let item = load_built_in_registry_item(&entry.name).map_err(|error| error.to_string())?;
        for file in &item.targets.ui_files {
            read_built_in_registry_source(&file.source).map_err(|error| error.to_string())?;
        }
        for style in &item.targets.style_blocks {
            read_built_in_registry_source(&style.source).map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn build_doctor_output(cwd: &Path, strict: bool, check: bool, trunk_build: bool) -> DoctorOutput {
    let mut checks = Vec::new();

    match build_info_output(cwd) {
        Ok(info) => {
            checks.push(DoctorCheck::pass(
                "project",
                "supported Trunk CSR project detected",
            ));
            if info.kit_config.is_some() {
                checks.push(DoctorCheck::pass("config", "kit.json is valid"));
            } else {
                checks.push(strict_check(
                    strict,
                    "config",
                    "kit.json is missing; run leptos_ui_kit init",
                ));
            }

            dependency_check(
                &mut checks,
                strict,
                "dependency.leptos",
                "leptos",
                info.detected.dependency_plan.leptos.status,
            );
            checks.extend(lock_checks(cwd, strict, info.kit_config.as_ref()));
            checks.extend(stylesheet_checks(cwd, strict, &info));
            checks.extend(registry_dependency_checks(cwd, strict, &info));
        }
        Err(error) => {
            checks.push(DoctorCheck::fail("project", error.to_string()));
        }
    }

    match validate_built_in_registry_assets() {
        Ok(()) => checks.push(DoctorCheck::pass(
            "registry",
            "all built-in registry assets are available",
        )),
        Err(error) => checks.push(DoctorCheck::fail("registry", error)),
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

fn lock_checks(cwd: &Path, strict: bool, kit_config: Option<&KitConfig>) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let lock_logical_path = kit_config
        .map(install_lock_path)
        .unwrap_or_else(|| DEFAULT_KIT_LOCK_PATH.to_owned());
    let lock_path = cwd.join(&lock_logical_path);
    if !lock_path.is_file() {
        checks.push(strict_check(
            strict,
            "lock",
            format!("{lock_logical_path} is missing"),
        ));
        return checks;
    }

    let lock_input = match fs::read_to_string(&lock_path) {
        Ok(input) => input,
        Err(error) => {
            checks.push(
                DoctorCheck::fail("lock", format!("failed to read lock: {error}"))
                    .with_path(lock_path.display().to_string()),
            );
            return checks;
        }
    };
    let lock = match parse_install_lock_str_at_path(&lock_input, Path::new(&lock_logical_path)) {
        Ok(lock) => lock,
        Err(error) => {
            checks.push(
                DoctorCheck::fail("lock", error.to_string())
                    .with_path(lock_path.display().to_string()),
            );
            return checks;
        }
    };

    checks.push(DoctorCheck::pass("lock", "install lock is valid"));
    checks.push(compare_config_hash(cwd, strict, &lock));
    if let Some(config) = kit_config {
        checks.extend(compare_desired_items(
            config,
            &lock,
            strict,
            &lock_logical_path,
        ));
    }
    for item in lock.items.values() {
        checks.push(compare_item_content_hash(item));
        for file in &item.files {
            let source_path = cwd.join(&file.path);
            checks.extend(compare_file_to_lock(
                "installed_file",
                file,
                &source_path,
                strict,
            ));
        }
        for block in &item.style_blocks {
            let css_path = cwd.join(&block.css_path);
            checks.extend(compare_css_block_to_lock(
                block,
                &css_path,
                &block.block_id,
                strict,
            ));
        }
    }
    checks.extend(git_metadata_checks(cwd, strict, &lock_logical_path));

    checks
}

fn stylesheet_checks(cwd: &Path, strict: bool, info: &InfoOutput) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let css_logical_path = info
        .kit_config
        .as_ref()
        .map(|config| config.styles.css.as_str())
        .unwrap_or(DEFAULT_CSS_PATH);
    let css_path = cwd.join(css_logical_path);

    if css_path.is_file() {
        checks.push(
            DoctorCheck::pass("stylesheet", format!("{css_logical_path} exists"))
                .with_path(css_path.display().to_string()),
        );
    } else {
        checks.push(
            strict_check(
                strict,
                "stylesheet",
                format!("{css_logical_path} is missing; run leptos_ui_kit init or sync"),
            )
            .with_path(css_path.display().to_string()),
        );
    }

    match fs::read_to_string(&info.detected.index_html_path) {
        Ok(html) if contains_trunk_css_link(&html, css_logical_path) => {
            checks.push(
                DoctorCheck::pass(
                    "stylesheet_link",
                    format!("index.html links {css_logical_path} for Trunk"),
                )
                .with_path(info.detected.index_html_path.display().to_string()),
            );
        }
        Ok(_) => checks.push(
            strict_check(
                strict,
                "stylesheet_link",
                format!("index.html is missing a Trunk CSS link for {css_logical_path}"),
            )
            .with_path(info.detected.index_html_path.display().to_string()),
        ),
        Err(error) => checks.push(
            DoctorCheck::fail(
                "stylesheet_link",
                format!("failed to read index.html: {error}"),
            )
            .with_path(info.detected.index_html_path.display().to_string()),
        ),
    }

    checks
}

fn contains_trunk_css_link(html: &str, css_path: &str) -> bool {
    html.lines().any(|line| {
        line.contains("data-trunk")
            && line.contains("rel=\"css\"")
            && line.contains(&format!("href=\"{css_path}\""))
    })
}

fn registry_dependency_checks(cwd: &Path, strict: bool, info: &InfoOutput) -> Vec<DoctorCheck> {
    let cargo_plan = registry_cargo_plan(cwd, info);
    if cargo_plan.is_empty() {
        return Vec::new();
    }

    match detect_cargo_plan_requirements(cwd, &cargo_plan) {
        Ok(requirements) => requirements
            .iter()
            .map(|requirement| registry_dependency_check(strict, requirement))
            .collect(),
        Err(error) => vec![DoctorCheck::fail(
            "dependency.registry",
            format!("failed to inspect registry dependency plan: {error}"),
        )],
    }
}

fn registry_cargo_plan(cwd: &Path, info: &InfoOutput) -> Vec<CargoPlanEntry> {
    let mut cargo_plan = Vec::new();

    if let Some(config) = info.kit_config.as_ref() {
        for item in &config.items {
            if let Ok(registry_item) = load_built_in_registry_item(item.item_name()) {
                merge_cargo_plan(&mut cargo_plan, &registry_item.item.cargo_plan);
            }
        }
    }

    if cargo_plan.is_empty() {
        if let Some(lock) = read_installed_lock(cwd, info.kit_config.as_ref()) {
            for item in lock.items.values() {
                if let Ok(registry_item) = load_built_in_registry_item(&item.name) {
                    merge_cargo_plan(&mut cargo_plan, &registry_item.item.cargo_plan);
                }
            }
        }
    }

    cargo_plan
}

fn merge_cargo_plan(plan: &mut Vec<CargoPlanEntry>, entries: &[CargoPlanEntry]) {
    for entry in entries {
        let mut entry = entry.clone();
        entry.features.sort();
        entry.features.dedup();
        if !plan.contains(&entry) {
            plan.push(entry);
        }
    }
    plan.sort();
}

fn registry_dependency_check(strict: bool, requirement: &DependencyRequirement) -> DoctorCheck {
    let name = format!("dependency.registry.{}", requirement.crate_name);
    match requirement.status {
        DependencyStatus::Satisfied => DoctorCheck::pass(
            name,
            format!(
                "{} dependency satisfies registry plan",
                requirement.crate_name
            ),
        ),
        DependencyStatus::Missing if !requirement.required => DoctorCheck::pass(
            name,
            format!(
                "optional {} dependency is not present",
                requirement.crate_name
            ),
        ),
        DependencyStatus::Missing => strict_check(
            strict,
            name,
            format!(
                "{} dependency required by registry plan is missing",
                requirement.crate_name
            ),
        ),
        DependencyStatus::Incompatible => strict_check(
            strict,
            name,
            format!(
                "{} dependency does not satisfy registry plan",
                requirement.crate_name
            ),
        ),
    }
}

fn compare_config_hash(cwd: &Path, strict: bool, lock: &InstallLock) -> DoctorCheck {
    let path = cwd.join(DEFAULT_KIT_CONFIG_PATH);
    match fs::read(&path) {
        Ok(content) if hash_content_bytes(&content) == lock.project.config_hash => {
            DoctorCheck::pass("config_hash", "kit.json hash matches install lock")
                .with_path(path.display().to_string())
        }
        Ok(_) => strict_check(
            strict,
            "config_hash",
            "kit.json hash differs from install lock",
        )
        .with_path(path.display().to_string()),
        Err(error) => DoctorCheck::fail("config_hash", format!("failed to read config: {error}"))
            .with_path(path.display().to_string()),
    }
}

fn compare_desired_items(
    config: &KitConfig,
    lock: &InstallLock,
    strict: bool,
    lock_logical_path: &str,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let desired_ids = config
        .items
        .iter()
        .map(|item| format!("builtin:{}", item.item_name()))
        .collect::<BTreeSet<_>>();

    for desired_id in &desired_ids {
        if lock.items.contains_key(desired_id) {
            checks.push(DoctorCheck::pass(
                "desired_item",
                format!("desired item {desired_id} is installed"),
            ));
        } else {
            checks.push(
                strict_check(
                    strict,
                    "desired_item",
                    format!("desired item {desired_id} is not installed"),
                )
                .with_path(DEFAULT_KIT_CONFIG_PATH),
            );
        }
    }

    for installed_id in lock.items.keys() {
        if !desired_ids.contains(installed_id) {
            checks.push(
                strict_check(
                    strict,
                    "desired_item",
                    format!("installed item {installed_id} is not declared in kit.json"),
                )
                .with_path(lock_logical_path),
            );
        }
    }

    checks
}

fn git_metadata_checks(cwd: &Path, strict: bool, state_logical_path: &str) -> Vec<DoctorCheck> {
    if !is_git_worktree(cwd) {
        return Vec::new();
    }

    let paths = BTreeSet::from([state_logical_path.to_owned()]);
    let mut ignored = Vec::new();
    for path in paths {
        match git_check_ignore(cwd, &path) {
            GitIgnoreStatus::Ignored => ignored.push(path),
            GitIgnoreStatus::NotIgnored => {}
            GitIgnoreStatus::Unknown(message) => {
                return vec![DoctorCheck::warning("git_metadata", message)];
            }
        }
    }

    if ignored.is_empty() {
        vec![DoctorCheck::pass(
            "git_metadata",
            "installer metadata is not ignored by Git",
        )]
    } else {
        ignored
            .into_iter()
            .map(|path| {
                strict_check(
                    strict,
                    "git_metadata",
                    format!("installer metadata {path} is ignored by Git"),
                )
                .with_path(path)
            })
            .collect()
    }
}

fn is_git_worktree(cwd: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "true")
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitIgnoreStatus {
    Ignored,
    NotIgnored,
    Unknown(String),
}

fn git_check_ignore(cwd: &Path, path: &str) -> GitIgnoreStatus {
    match Command::new("git")
        .args(["check-ignore", "-q", path])
        .current_dir(cwd)
        .status()
    {
        Ok(status) if status.success() => GitIgnoreStatus::Ignored,
        Ok(status) if status.code() == Some(1) => GitIgnoreStatus::NotIgnored,
        Ok(status) => GitIgnoreStatus::Unknown(format!(
            "failed to check Git ignore status for {path}: exit status {}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        )),
        Err(error) => GitIgnoreStatus::Unknown(format!(
            "failed to run git check-ignore for {path}: {error}"
        )),
    }
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

fn compare_file_to_lock(
    name: &str,
    file: &InstalledFile,
    source_path: &Path,
    _strict: bool,
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
    let source_hash = hash_content_bytes(source.as_bytes());
    let mut checks = Vec::new();

    if source_hash == file.generated_hash {
        checks.push(
            DoctorCheck::pass(name, "installed file matches generated source")
                .with_path(source_path.display().to_string()),
        );
    } else {
        checks.push(
            DoctorCheck::warning(name, "installed file has local edits")
                .with_path(source_path.display().to_string()),
        );
    }

    if source_hash == file.local_hash_at_install {
        checks.push(
            DoctorCheck::pass(
                "installed_file_hash",
                "installed file hash matches install lock",
            )
            .with_path(source_path.display().to_string()),
        );
    } else {
        checks.push(
            DoctorCheck::warning(
                "installed_file_hash",
                "installed file hash differs from lock",
            )
            .with_path(source_path.display().to_string()),
        );
    }

    checks
}

fn compare_css_block_to_lock(
    block: &InstalledStyleBlock,
    css_path: &Path,
    block_id: &str,
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
    let mut checks = Vec::new();

    match extract_managed_css_block(&css, block_id) {
        Ok(Some(current)) => {
            let current_hash = hash_content_bytes(current.as_bytes());
            if current_hash == block.generated_hash {
                checks.push(
                    DoctorCheck::pass(
                        "style_block",
                        format!("managed CSS block {block_id} matches generated source"),
                    )
                    .with_path(css_path.display().to_string()),
                );
            } else {
                checks.push(
                    DoctorCheck::warning(
                        "style_block",
                        format!("managed CSS block {block_id} has local edits"),
                    )
                    .with_path(css_path.display().to_string()),
                );
            }
        }
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
            DoctorCheckStatus::Warning => Some((DiagnosticLevel::Warning, check)),
            DoctorCheckStatus::Fail => Some((DiagnosticLevel::Error, check)),
        })
        .map(|(level, check)| {
            let diagnostic = Diagnostic::new(
                level,
                format!("doctor.{}", check.name),
                check.message.clone(),
            );
            check
                .path
                .clone()
                .map_or(diagnostic.clone(), |path| diagnostic.with_path(path))
        })
        .collect()
}

fn read_installed_lock(project_root: &Path, kit_config: Option<&KitConfig>) -> Option<InstallLock> {
    let state_logical_path = kit_config
        .map(install_lock_path)
        .unwrap_or_else(|| DEFAULT_KIT_LOCK_PATH.to_owned());
    let path = project_root.join(&state_logical_path);
    let input = fs::read_to_string(path).ok()?;
    parse_install_lock_str_at_path(&input, Path::new(&state_logical_path)).ok()
}

fn usage() -> String {
    "usage: leptos_ui_kit <add|doctor|info|init|sync|view> [--json] [--dry-run] [path-or-source]"
        .to_owned()
}

fn help_text() -> String {
    [
        "leptos_ui_kit",
        "",
        "usage: leptos_ui_kit <command> [options]",
        "",
        "commands:",
        "  info                 inspect a supported Trunk CSR Leptos app",
        "  init                 create src/components/ui/_kit/kit.json and kit-managed app files",
        "  view <item>          show a registry item",
        "  add <item>           add a registry item to the app",
        "  sync                 reconcile installed items with src/components/ui/_kit/kit.json",
        "  doctor               validate generated source, CSS, lock metadata, and dependencies",
        "",
        "global options:",
        "  --cwd <path>         run against a different project root",
        "  --quiet              accepted for script compatibility",
        "  --verbose            accepted for script compatibility",
        "  --help               print help",
        "  --version            print version",
    ]
    .join("\n")
}

fn command_help(command: &str) -> Result<String, String> {
    let lines = match command {
        "add" => vec![
            "usage: leptos_ui_kit add <item> [--dry-run] [--json]",
            "",
            "Adds a built-in registry item and updates src/components/ui/_kit/kit.json, src/components/ui/_kit/kit.lock.json, generated source, and CSS.",
        ],
        "doctor" => vec![
            "usage: leptos_ui_kit doctor [--strict] [--check] [--trunk-build] [--json]",
            "",
            "Validates project shape, dependencies, desired state, generated files, managed CSS, and installer metadata.",
        ],
        "info" => vec![
            "usage: leptos_ui_kit info [path] [--json]",
            "",
            "Inspects a supported single-crate Trunk CSR Leptos app.",
        ],
        "init" => vec![
            "usage: leptos_ui_kit init [--dry-run] [--json]",
            "",
            "Creates src/components/ui/_kit/kit.json, src/components/ui/_kit/kit.lock.json, and the minimal app-owned source and CSS files.",
        ],
        "sync" => vec![
            "usage: leptos_ui_kit sync [--dry-run] [--json]",
            "",
            "Reconciles installed source, CSS, and src/components/ui/_kit/kit.lock.json with src/components/ui/_kit/kit.json.",
        ],
        "view" => vec![
            "usage: leptos_ui_kit view <item> [--source] [--json]",
            "",
            "Shows a built-in registry item and optionally its source files.",
        ],
        _ => return Err(usage()),
    };
    Ok(lines.join("\n"))
}

fn current_dir() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use leptos_ui_kit_codegen::plan_init;
    use leptos_ui_kit_registry::{
        desired_builtin_button_item, kit_config_to_json, kit_config_with_desired_item,
        parse_kit_json_str,
    };
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
        fs::write(root.join("styles/kit.css"), ":root {}\n").expect("write css");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="css" href="styles/kit.css" />
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
        assert!(output.contains("\"cargoPlan\""));
        assert!(output.contains("\"source\""));
        assert!(output.contains("\"features\""));
    }

    #[test]
    fn view_envelope_json_outputs_css_only_tokens_item() {
        let item = load_registry_item("tokens", Path::new(".")).expect("load tokens item");
        let output = render_registry_item(&item, true, true).expect("render json");

        assert!(output.contains("\"name\": \"tokens\""));
        assert!(output.contains("\"kind\": \"foundation\""));
        assert!(output.contains("styles/tokens.css"));
        assert!(!output.contains("\"kind\": \"rust\""));
    }

    #[test]
    fn view_source_outputs_registry_source_contents() {
        let item = load_registry_item("button", Path::new(".")).expect("load built-in item");
        let output = render_registry_item(&item, true, true).expect("render json");

        assert!(output.contains("\"sources\""));
        assert!(output.contains("pub fn Button"));
        assert!(output.contains(".kit-button"));
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
        assert!(output.contains("\"path\": \"src/components/ui/_kit/kit.json\""));
        assert!(!root.join(DEFAULT_KIT_CONFIG_PATH).exists());
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

        assert!(root.join(DEFAULT_KIT_CONFIG_PATH).is_file());
        assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());
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
        assert!(output.contains("\"cargoPlan\""));
        assert!(output.contains("\"crate\": \"leptos\""));
        assert!(output.contains("\"path\": \"src/components/ui/button.rs\""));
        assert!(output.contains("\"path\": \"src/components/ui/_kit/kit.lock.json\""));
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
        assert!(root.join(DEFAULT_KIT_LOCK_PATH).is_file());

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
    fn sync_dry_run_envelope_json_outputs_declared_button_changes() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        run(
            vec![
                OsString::from("sync"),
                OsString::from("--dry-run"),
                OsString::from("--json"),
            ],
            root,
        )
        .expect("run sync dry-run");

        let output = render_sync_plan(
            &plan_sync(root).expect("plan sync"),
            true,
            CommandStatus::Planned,
        )
        .expect("render sync");
        assert!(output.contains("\"command\": \"sync\""));
        assert!(output.contains("\"status\": \"planned\""));
        assert!(output.contains("\"itemIds\": ["));
        assert!(output.contains("\"builtin:button\""));
        assert!(output.contains("\"cargoPlan\""));
        assert!(output.contains("\"crate\": \"leptos\""));
        assert!(!output.contains("\"crate\": \"leptos_router\""));
        assert!(output.contains("\"path\": \"src/components/ui/button.rs\""));
    }

    #[test]
    fn sync_write_installs_declared_button_and_then_reports_no_change() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("src")).expect("create src");
        fs::write(
            root.join("index.html"),
            "<html><head></head><body></body></html>\n",
        )
        .expect("write index");
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        run(vec![OsString::from("sync")], root).expect("run sync");
        assert!(root.join("src/components/ui/button.rs").is_file());

        run(vec![OsString::from("sync")], root).expect("run second sync");
        let output = render_sync_plan(
            &plan_sync(root).expect("plan sync"),
            true,
            CommandStatus::NoChange,
        )
        .expect("render sync");
        assert!(output.contains("\"status\": \"no_change\""));
    }

    #[test]
    fn doctor_strict_passes_after_sync_reconciles_button_dependencies() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        run(vec![OsString::from("sync")], root).expect("run sync");
        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Success);
        assert!(output.contains("managed CSS block tokens matches generated source"));
        assert!(output.contains("desired item builtin:tokens is installed"));
        assert!(output.contains("desired item builtin:spinner is installed"));
        assert!(output.contains("desired item builtin:button is installed"));
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
        assert!(output.contains("\"name\": \"dependency.registry.leptos\""));
        assert!(!output.contains("\"name\": \"dependency.registry.leptos_router\""));
    }

    #[test]
    fn doctor_strict_passes_after_tokens_only_add_without_router() {
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
        run(vec![OsString::from("add"), OsString::from("tokens")], root).expect("run add tokens");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Success);
        assert!(output.contains("all built-in registry assets are available"));
        assert!(!output.contains("\"name\": \"dependency.leptos_router\""));
        assert!(!output.contains("\"name\": \"dependency.registry.leptos_router\""));
    }

    #[test]
    fn doctor_diagnostics_preserve_each_duplicate_check_path() {
        let output = DoctorOutput {
            project_root: PathBuf::from("."),
            strict: true,
            check: false,
            trunk_build: false,
            checks: vec![
                DoctorCheck::warning("style_block", "first").with_path("first.css"),
                DoctorCheck::warning("style_block", "second").with_path("second.css"),
            ],
        };
        let diagnostics = doctor_diagnostics(&output);

        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].path.as_deref(), Some("first.css"));
        assert_eq!(diagnostics[1].path.as_deref(), Some("second.css"));
    }

    #[test]
    fn doctor_strict_fails_when_desired_item_is_not_installed() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        run(vec![OsString::from("init")], root).expect("run init");
        write_desired_button_config(root);

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.desired_item\""));
        assert!(output.contains("desired item builtin:button is not installed"));
    }

    #[test]
    fn doctor_strict_fails_when_installed_item_is_not_desired() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");
        write_empty_items_config(root);

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.desired_item\""));
        assert!(output.contains("installed item builtin:button is not declared in kit.json"));
    }

    #[test]
    fn doctor_strict_fails_when_installer_metadata_is_ignored() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        create_doctor_project(root);
        init_git(root);
        fs::write(
            root.join(".gitignore"),
            "/src/components/ui/_kit/kit.lock.json\n",
        )
        .expect("write gitignore");
        run(vec![OsString::from("init")], root).expect("run init");
        run(vec![OsString::from("add"), OsString::from("button")], root).expect("run add");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Error);
        assert!(output.contains("\"code\": \"doctor.git_metadata\""));
        assert!(
            output.contains(
                "installer metadata src/components/ui/_kit/kit.lock.json is ignored by Git"
            )
        );
    }

    #[test]
    fn doctor_reports_lock_hash_mismatches_as_warnings() {
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

        let lock_path = root.join(DEFAULT_KIT_LOCK_PATH);
        let mut lock: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&lock_path).expect("read lock"))
                .expect("parse lock");
        lock["items"]["builtin:button"]["files"][0]["generatedHash"] =
            serde_json::Value::String(format!("sha256:{}", "0".repeat(64)));
        fs::write(
            &lock_path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&lock).expect("serialize lock")
            ),
        )
        .expect("write lock");

        let doctor = build_doctor_output(root, true, false, false);
        let output =
            render_doctor_output(&doctor, true, doctor_status(&doctor)).expect("render doctor");

        assert_eq!(doctor_status(&doctor), CommandStatus::Warning);
        assert!(output.contains("\"code\": \"doctor.installed_file\""));
        assert!(output.contains("installed file has local edits"));
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

        let css_path = root.join("styles/kit.css");
        let mut css = fs::read_to_string(&css_path).expect("read css");
        let block = extract_managed_css_block(&css, "button")
            .expect("extract block")
            .expect("button block");
        css.push('\n');
        css.push_str(&block);
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
    fn help_and_version_flags_return_success() {
        run(vec![OsString::from("--help")], Path::new(".")).expect("top-level help");
        run(vec![OsString::from("--version")], Path::new(".")).expect("version");
        run(
            vec![OsString::from("--version"), OsString::from("--json")],
            Path::new("."),
        )
        .expect("json version");
        run(
            vec![OsString::from("sync"), OsString::from("--help")],
            Path::new("."),
        )
        .expect("command help");
    }

    #[test]
    fn version_json_outputs_tool_provenance() {
        let output = render_version_output(true).expect("render version");

        assert!(output.contains("\"command\": \"version\""));
        assert!(output.contains("\"package\": \"leptos_ui_kit_cli\""));
        assert!(output.contains("\"binary\": \"leptos_ui_kit\""));
        assert!(output.contains("\"version\": \"0.1.0\""));
        assert!(output.contains("\"schemaVersion\": \"0.9.0-alpha\""));
        assert!(output.contains("\"kind\": \"git\""));
        assert!(output.contains("\"url\": \"https://github.com/triesap/leptos_ui_kit\""));
    }

    #[test]
    fn version_rejects_unknown_flags() {
        let error = run(
            vec![OsString::from("--version"), OsString::from("--source")],
            Path::new("."),
        )
        .expect_err("version flag should be unsupported");

        assert!(error.contains("unsupported flag for version"));
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
        fs::write(root.join("styles/kit.css"), ":root {}\n").expect("write css");
        fs::write(
            root.join("index.html"),
            r#"<!DOCTYPE html>
<html>
  <head>
    <link data-trunk rel="css" href="styles/kit.css" />
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
                "cannot safely patch src/components/ui/button.rs: target exists but is not tracked in lock"
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

    fn create_doctor_project(root: &Path) {
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
    }

    fn init_git(root: &Path) {
        let output = Command::new("git")
            .arg("init")
            .current_dir(root)
            .output()
            .expect("run git init");

        assert!(
            output.status.success(),
            "git init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_desired_button_config(root: &Path) {
        let config = parse_kit_json_str(
            &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
        )
        .expect("parse config");
        let config = kit_config_with_desired_item(config, desired_builtin_button_item())
            .expect("add desired item");
        fs::write(
            root.join(DEFAULT_KIT_CONFIG_PATH),
            kit_config_to_json(&config).expect("serialize config"),
        )
        .expect("write config");
    }

    fn write_empty_items_config(root: &Path) {
        let mut config = parse_kit_json_str(
            &fs::read_to_string(root.join(DEFAULT_KIT_CONFIG_PATH)).expect("read config"),
        )
        .expect("parse config");
        config.items.clear();
        fs::write(
            root.join(DEFAULT_KIT_CONFIG_PATH),
            kit_config_to_json(&config).expect("serialize config"),
        )
        .expect("write config");
    }
}
