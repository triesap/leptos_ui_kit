use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use syn::{Item, ItemMod, ItemUse, UseTree, Visibility};

use super::unsafe_patch;
use crate::{CodegenError, UiModuleExport};

const MAX_MODULE_SOURCE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct UseBinding {
    source: Vec<String>,
    exported: String,
    public: bool,
}

#[derive(Debug, Default)]
struct ModuleFacts {
    modules: Vec<ModuleDeclaration>,
    uses: Vec<UseBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModuleDeclaration {
    name: String,
    public: bool,
    external: bool,
}

pub fn patch_components_mod(existing: Option<&str>) -> Result<String, CodegenError> {
    patch_components_mod_at_path(existing, "src/components/mod.rs", "ui")
}

pub(crate) fn patch_components_mod_at_path(
    existing: Option<&str>,
    logical_path: &str,
    ui_module_name: &str,
) -> Result<String, CodegenError> {
    validate_patch_identifier(ui_module_name, "UI module name", Path::new(logical_path))?;
    patch_module_file(
        existing.unwrap_or_default(),
        logical_path,
        &[ui_module_name],
        &[],
    )
}

pub fn patch_ui_mod(
    existing: Option<&str>,
    exports: &[UiModuleExport],
) -> Result<String, CodegenError> {
    patch_ui_mod_at_path(existing, exports, "src/components/ui/mod.rs")
}

pub(crate) fn patch_ui_mod_at_path(
    existing: Option<&str>,
    exports: &[UiModuleExport],
    logical_path: &str,
) -> Result<String, CodegenError> {
    for export in exports {
        validate_patch_identifier(&export.module, "UI module name", Path::new(logical_path))?;
        validate_module_path(&export.path, "UI export path", Path::new(logical_path))?;
        for symbol in &export.symbols {
            validate_patch_identifier(symbol, "UI export symbol", Path::new(logical_path))?;
        }
    }

    let required_modules = exports
        .iter()
        .map(|export| export.module.as_str())
        .collect::<Vec<_>>();
    patch_module_file(
        existing.unwrap_or_default(),
        logical_path,
        &required_modules,
        exports,
    )
}

fn patch_module_file(
    existing: &str,
    logical_path: &str,
    required_modules: &[&str],
    required_exports: &[UiModuleExport],
) -> Result<String, CodegenError> {
    let facts = inspect_module_file(existing, logical_path)?;
    let mut appended = Vec::new();
    let mut seen_modules = BTreeSet::new();
    let mut satisfied_bindings = facts
        .uses
        .iter()
        .filter(|binding| binding.public)
        .map(|binding| (binding.source.clone(), binding.exported.clone()))
        .collect::<BTreeSet<_>>();

    if required_exports.is_empty() {
        for module_name in required_modules {
            plan_required_module(
                &facts,
                &mut seen_modules,
                &mut appended,
                logical_path,
                module_name,
            )?;
        }
    }

    for export in required_exports {
        plan_required_module(
            &facts,
            &mut seen_modules,
            &mut appended,
            logical_path,
            &export.module,
        )?;
        let source_prefix = export.path.split("::").collect::<Vec<_>>();
        let missing = export
            .symbols
            .iter()
            .filter_map(|symbol| {
                let mut required_source = source_prefix
                    .iter()
                    .map(|segment| (*segment).to_owned())
                    .collect::<Vec<_>>();
                required_source.push(symbol.clone());

                if satisfied_bindings.contains(&(required_source.clone(), symbol.clone())) {
                    return None;
                }

                Some((symbol, required_source))
            })
            .collect::<Vec<_>>();

        for (symbol, required_source) in &missing {
            if facts.uses.iter().any(|binding| {
                binding.exported.as_str() == symbol.as_str()
                    && (!binding.public || binding.source.as_slice() != required_source.as_slice())
            }) {
                return unsafe_patch(
                    logical_path,
                    format!(
                        "private or incompatible use declaration blocks required public export `{}`",
                        symbol
                    ),
                );
            }
        }

        let missing_symbols = missing
            .iter()
            .map(|(symbol, _)| (*symbol).clone())
            .collect::<Vec<_>>();
        match missing_symbols.as_slice() {
            [] => {}
            [symbol] => appended.push(format!("pub use {}::{symbol};", export.path)),
            symbols => appended.push(format_grouped_pub_use(&export.path, symbols)),
        }
        for (symbol, source) in missing {
            satisfied_bindings.insert((source, symbol.clone()));
        }
    }

    append_module_lines(existing, logical_path, &appended)
}

fn plan_required_module(
    facts: &ModuleFacts,
    seen_modules: &mut BTreeSet<String>,
    appended: &mut Vec<String>,
    logical_path: &str,
    module_name: &str,
) -> Result<(), CodegenError> {
    if !seen_modules.insert(module_name.to_owned()) {
        return Ok(());
    }
    match module_declaration_state(facts, module_name) {
        DeclarationState::Present => Ok(()),
        DeclarationState::Missing => {
            appended.push(format!("pub mod {module_name};"));
            Ok(())
        }
        DeclarationState::Conflict => unsafe_patch(
            logical_path,
            format!(
                "private or incompatible module declaration blocks required public module `{module_name}`"
            ),
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclarationState {
    Present,
    Missing,
    Conflict,
}

fn module_declaration_state(facts: &ModuleFacts, required: &str) -> DeclarationState {
    let matching = facts
        .modules
        .iter()
        .filter(|declaration| declaration.name == required)
        .collect::<Vec<_>>();
    if matching.is_empty() {
        DeclarationState::Missing
    } else if matching
        .iter()
        .all(|declaration| declaration.public && declaration.external)
    {
        DeclarationState::Present
    } else {
        DeclarationState::Conflict
    }
}

fn inspect_module_file(existing: &str, logical_path: &str) -> Result<ModuleFacts, CodegenError> {
    if existing.len() > MAX_MODULE_SOURCE_BYTES {
        return unsafe_patch(
            logical_path,
            format!(
                "module source exceeds the {MAX_MODULE_SOURCE_BYTES}-byte additive inspection limit"
            ),
        );
    }
    let syntax = syn::parse_file(existing).map_err(|error| CodegenError::UnsafePatch {
        path: PathBuf::from(logical_path),
        reason: format!("module source must parse as Rust before additive patching: {error}"),
    })?;
    let mut facts = ModuleFacts::default();

    for item in syntax.items {
        match item {
            Item::Mod(item_mod) => facts.modules.push(module_declaration(item_mod)),
            Item::Use(item_use) => collect_item_use(item_use, &mut facts.uses),
            _ => {}
        }
    }

    Ok(facts)
}

fn module_declaration(item: ItemMod) -> ModuleDeclaration {
    ModuleDeclaration {
        name: item.ident.to_string(),
        public: is_public(&item.vis),
        external: item.content.is_none(),
    }
}

fn collect_item_use(item: ItemUse, bindings: &mut Vec<UseBinding>) {
    let mut prefix = Vec::new();
    if item.leading_colon.is_some() {
        prefix.push(String::new());
    }
    collect_use_tree(&item.tree, &mut prefix, is_public(&item.vis), bindings);
}

fn collect_use_tree(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    public: bool,
    bindings: &mut Vec<UseBinding>,
) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_tree(&path.tree, prefix, public, bindings);
            prefix.pop();
        }
        UseTree::Name(name) => {
            let name = name.ident.to_string();
            let (source, exported) = if name == "self" {
                (
                    prefix.clone(),
                    prefix.last().cloned().unwrap_or_else(|| "self".to_owned()),
                )
            } else {
                let mut source = prefix.clone();
                source.push(name.clone());
                (source, name)
            };
            bindings.push(UseBinding {
                source,
                exported,
                public,
            });
        }
        UseTree::Rename(rename) => {
            let mut source = prefix.clone();
            let source_name = rename.ident.to_string();
            if source_name != "self" {
                source.push(source_name);
            }
            bindings.push(UseBinding {
                source,
                exported: rename.rename.to_string(),
                public,
            });
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_tree(tree, prefix, public, bindings);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn is_public(visibility: &Visibility) -> bool {
    matches!(visibility, Visibility::Public(_))
}

fn append_module_lines(
    existing: &str,
    logical_path: &str,
    lines: &[String],
) -> Result<String, CodegenError> {
    if lines.is_empty() {
        return Ok(existing.to_owned());
    }
    if lines
        .iter()
        .any(|line| line.is_empty() || line.trim() != line)
    {
        return unsafe_patch(logical_path, "module patch line must be normalized");
    }

    let mut output = String::with_capacity(
        existing.len() + lines.iter().map(String::len).sum::<usize>() + lines.len() + 1,
    );
    output.push_str(existing);
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    for line in lines {
        output.push_str(line);
        output.push('\n');
    }
    Ok(output)
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
