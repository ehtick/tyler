#![allow(
    clippy::manual_is_multiple_of,
    clippy::needless_as_bytes,
    clippy::needless_borrows_for_generic_args,
    clippy::to_string_trait_impl,
    clippy::too_many_arguments,
    clippy::unnecessary_fallible_conversions,
    clippy::unnecessary_to_owned,
    clippy::unnecessary_unwrap,
    clippy::useless_vec
)]

pub mod gltf_writer;
mod proj;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use cityjson_lib::CityModel;
use log::info;

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct ExportOptions {
    pub native_glb_color: String,
    pub metadata_class_name: String,
    pub feature_type_colors: BTreeMap<String, String>,
    pub source_crs: Option<String>,
    pub ecef_origin: Option<[f64; 3]>,
    pub clip_bbox: Option<[f64; 6]>,
    pub reproject_to_ecef: bool,
    pub smooth_normals: bool,
    pub quantize_geometry: bool,
    pub meshopt_compression: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            native_glb_color: "#FFC0CB".to_string(),
            metadata_class_name: "cityobject".to_string(),
            feature_type_colors: BTreeMap::new(),
            source_crs: None,
            ecef_origin: None,
            clip_bbox: None,
            reproject_to_ecef: true,
            smooth_normals: false,
            quantize_geometry: true,
            meshopt_compression: true,
        }
    }
}

/// Converts a `CityJSON` model to a GLB file.
///
/// # Errors
///
/// Returns an error when the output directory cannot be created or GLB writing
/// fails.
pub fn convert_to_glb<P: AsRef<Path>>(
    model: &CityModel,
    output: P,
    options: &ExportOptions,
) -> Result<()> {
    let output = output.as_ref();
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    info!("Writing GLB output to {}", output.display());
    gltf_writer::write_city_model_glb(model, output, options)
}
