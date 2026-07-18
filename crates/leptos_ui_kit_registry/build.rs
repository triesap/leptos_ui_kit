mod build_assets;
mod build_provenance;

use std::{
    env,
    path::{Path, PathBuf},
};

use build_assets::generate_asset_catalog;
use build_provenance::{
    SystemGit, explicit_revision, probe_checkout, read_cargo_vcs, resolve_provenance,
};

const REV_ENV: &str = "LEPTOS_UI_KIT_GIT_REV";
const SOURCE_ENV: &str = "LEPTOS_UI_KIT_GIT_REV_SOURCE";

fn main() {
    if let Err(error) = emit_asset_catalog() {
        panic!("failed to generate leptos_ui_kit embedded assets: {error}");
    }
    if let Err(error) = emit_build_provenance() {
        panic!("failed to resolve leptos_ui_kit build provenance: {error}");
    }
}

fn emit_asset_catalog() -> Result<(), build_assets::AssetCatalogError> {
    println!("cargo:rerun-if-changed=build_assets.rs");
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR to registry build scripts"),
    );
    let catalog = generate_asset_catalog(package_root, &out_dir)?;
    for path in catalog.rerun_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(())
}

fn emit_build_provenance() -> Result<(), build_provenance::ProvenanceError> {
    println!("cargo:rerun-if-env-changed={REV_ENV}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_provenance.rs");

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let explicit = env::var_os(REV_ENV);
    if explicit.is_some() {
        let resolved = resolve_provenance(explicit_revision(explicit.as_deref())?, None, None)?;
        emit_resolved(resolved.as_ref());
        return Ok(());
    }

    let cargo_vcs_path = manifest_dir.join(".cargo_vcs_info.json");
    println!("cargo:rerun-if-changed={}", cargo_vcs_path.display());
    if let Some(cargo_vcs) = read_cargo_vcs(&cargo_vcs_path)? {
        let resolved = resolve_provenance(None, Some(&cargo_vcs), None)?;
        emit_resolved(resolved.as_ref());
        return Ok(());
    }

    let probe = probe_checkout(manifest_dir, &mut SystemGit);
    for path in probe.rerun_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let resolved = resolve_provenance(
        None,
        None,
        probe
            .checkout
            .as_ref()
            .map(|checkout| checkout.as_borrowed()),
    )?;
    emit_resolved(resolved.as_ref());
    Ok(())
}

fn emit_resolved(resolved: Option<&build_provenance::ResolvedProvenance>) {
    if let Some(resolved) = resolved {
        println!("cargo:rustc-env={REV_ENV}={}", resolved.rev);
        println!(
            "cargo:rustc-env={SOURCE_ENV}={}",
            resolved.source.as_env_value()
        );
    } else {
        println!("cargo:rustc-env={SOURCE_ENV}=unavailable");
    }
}
