mod css;
mod html;
mod module;

pub(crate) use css::{ManagedCssRetirement, reconcile_managed_css_blocks_with_retirements_at_path};
pub use css::{
    extract_managed_css_block, extract_managed_css_block_at_path,
    inspect_managed_css_blocks_at_path, patch_css_block, patch_css_block_at_path,
    reconcile_managed_css_blocks_at_path,
};
pub(crate) use html::plan_index_html;
pub use module::{patch_components_mod, patch_ui_mod};
pub(crate) use module::{patch_components_mod_at_path, patch_ui_mod_at_path};

use std::path::PathBuf;

use crate::CodegenError;

pub(crate) fn unsafe_patch<T>(
    path: impl Into<PathBuf>,
    reason: impl Into<String>,
) -> Result<T, CodegenError> {
    Err(CodegenError::UnsafePatch {
        path: path.into(),
        reason: reason.into(),
    })
}
