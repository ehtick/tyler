#![allow(
    clippy::manual_is_multiple_of,
    clippy::needless_as_bytes,
    clippy::needless_borrows_for_generic_args,
    clippy::to_string_trait_impl,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_fallible_conversions,
    clippy::unnecessary_to_owned,
    clippy::unnecessary_unwrap,
    clippy::useless_vec
)]

#[path = "../../src/parser.rs"]
pub mod parser;
#[path = "../../src/proj.rs"]
pub mod proj;
#[path = "../../src/spatial_structs.rs"]
pub mod spatial_structs;

pub mod gltf_writer;

use std::fs;
use std::path::Path;

use anyhow::Result;
use cityjson_index::CityIndex;
use log::info;

pub struct ExportOptions {
    pub object_types: Option<Vec<parser::CityObjectType>>,
    pub grid_cellsize: u32,
    pub grid_minz: Option<i32>,
    pub grid_maxz: Option<i32>,
    pub native_glb_color: String,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            object_types: None,
            grid_cellsize: 250,
            grid_minz: None,
            grid_maxz: None,
            native_glb_color: "#FFC0CB".to_string(),
        }
    }
}

pub fn export_glb<P: AsRef<Path>, Q: AsRef<Path>>(
    input: P,
    output: Q,
    options: &ExportOptions,
) -> Result<()> {
    let input = input.as_ref();
    let output = output.as_ref();
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    let world = build_world(input, options)?;
    let quadtree = spatial_structs::QuadTree::from_world(
        &world,
        spatial_structs::QuadTreeCapacity::Vertices(usize::MAX),
    );

    info!("Writing GLB output to {}", output.display());
    gltf_writer::write_tile_glb(
        &world,
        &quadtree,
        quadtree.id.clone(),
        output,
        &options.native_glb_color,
    )?;

    Ok(())
}

fn build_world(input: &Path, options: &ExportOptions) -> Result<parser::World> {
    match cityjson_index::resolve_dataset(input, None) {
        Ok(resolved) => {
            let inspection = resolved.inspect()?;
            let mut city_index = CityIndex::open(resolved.storage_layout(), &resolved.index_path)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if !inspection.index.exists || inspection.index.fresh != Some(true) {
                info!(
                    "Rebuilding cjindex sidecar at {}",
                    resolved.index_path.display()
                );
                city_index.reindex()?;
            }
            let feature_base_document = derive_base_document(&city_index)?;
            let metadata_path = input.join("metadata.city.json");

            let mut world = parser::World::from_cjindex(
                parser::InputSource::from_cjindex_resolved(&resolved),
                metadata_path,
                feature_base_document,
                options.grid_cellsize,
                options.object_types.clone(),
                options.grid_minz,
                options.grid_maxz,
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            world
                .index_with_grid()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            Ok(world)
        }
        Err(_error) => {
            let metadata_path = input.join("metadata.city.json");
            if !metadata_path.is_file() {
                return Err(anyhow::anyhow!(
                    "{} is neither a cjindex dataset root nor a legacy dataset root containing metadata.city.json",
                    input.display()
                ));
            }

            let mut world = parser::World::new(
                metadata_path.clone(),
                input.to_path_buf(),
                options.grid_cellsize,
                options.object_types.clone(),
                options.grid_minz,
                options.grid_maxz,
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            world
                .index_with_grid()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            Ok(world)
        }
    }
}

fn derive_base_document(city_index: &CityIndex) -> Result<Vec<u8>> {
    let metadata = city_index.metadata()?;
    let Some(base_document) = metadata.first() else {
        return Err(anyhow::anyhow!(
            "cjindex dataset does not contain any source metadata"
        ));
    };
    if metadata
        .iter()
        .skip(1)
        .any(|candidate| candidate.as_ref() != base_document.as_ref())
    {
        return Err(anyhow::anyhow!(
            "cjindex dataset contains multiple metadata documents; a single shared base document is required"
        ));
    }
    Ok(serde_json::to_vec(base_document.as_ref())?)
}
