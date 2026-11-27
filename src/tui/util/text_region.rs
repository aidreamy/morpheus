//! Re-export of the copy-by-selection text-region machinery.
//!
//! The implementation moved to `morpheus-tui` (`morpheus_tui::core::text_region`),
//! where it lives next to the rendering primitives it cooperates with
//! (`unicode_util::word_wrap`, the markdown renderer, cell buffers) and can
//! be reused by any host of the crate. The content-only region contract -
//! region `text` char 0 maps to screen column `x1`; full-width sampling
//! builders strip border/margin columns via `regions_from_full_width_cells`
//! - is documented there.
//!
//! Import from here (or straight from `morpheus_tui`) in the app; this shim
//! keeps the existing `crate::util::text_region` paths stable.
//!
//! `morpheus_tui::core::text_region` also exposes `regions_from_wrapped_lines`;
//! it is re-exported here once its call sites migrate.

pub use morpheus_tui::core::text_region::{
    KEEP_GLYPH, TextRegion, extract_text_in_region, regions_from_full_width_cells,
    text_from_cell_row,
};
