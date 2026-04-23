// Copyright 2023 Balázs Dukai, Ravi Peters
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
#![allow(
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cloned_instead_of_copied,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::explicit_iter_loop,
    clippy::if_not_else,
    clippy::assigning_clones,
    clippy::manual_string_new,
    clippy::manual_assert,
    clippy::manual_is_multiple_of,
    clippy::manual_midpoint,
    clippy::match_bool,
    clippy::match_same_arms,
    clippy::needless_as_bytes,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_pass_by_value,
    clippy::redundant_else,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::struct_excessive_bools,
    clippy::struct_field_names,
    clippy::to_string_trait_impl,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::type_complexity,
    clippy::unnecessary_fallible_conversions,
    clippy::unnecessary_debug_formatting,
    clippy::unnecessary_semicolon,
    clippy::unnecessary_to_owned,
    clippy::unnecessary_unwrap,
    clippy::unnecessary_wraps,
    clippy::uninlined_format_args,
    clippy::unreadable_literal,
    clippy::used_underscore_binding,
    clippy::used_underscore_items,
    clippy::stable_sort_primitive,
    clippy::useless_vec
)]
mod cli;
mod formats;
mod parser;
mod proj;
mod spatial_structs;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::formats::cesium3dtiles::{Tile, TileId};
use cityjson_lib::cityjson::prelude::{CityObjectHandle, GeometryHandle};
use clap::Parser;
use log::{debug, info, log_enabled, warn, Level};
use rayon::prelude::*;

#[derive(Debug, Clone, clap::ValueEnum, Eq, PartialEq)]
#[clap(rename_all = "lower")]
pub enum Formats {
    _3DTiles,
    CityJSON,
}

impl ToString for Formats {
    fn to_string(&self) -> String {
        match self {
            Formats::_3DTiles => "3DTiles".to_string(),
            Formats::CityJSON => "CityJSON".to_string(),
        }
    }
}

#[derive(Default, Debug)]
struct DebugData {
    world: Option<PathBuf>,
    quadtree: Option<PathBuf>,
    tiles_results: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct PreparedInput {
    source: parser::InputSource,
    metadata_path: PathBuf,
    feature_base_document: Option<Vec<u8>>,
}

fn build_glb_export_options(
    cli: &crate::cli::Cli,
    source_crs: Option<String>,
    ecef_origin: Option<[f64; 3]>,
    clip_bbox: Option<[f64; 6]>,
) -> cityjson_convert::ExportOptions {
    let mut feature_type_colors = BTreeMap::new();

    for (feature_type, color) in [
        ("Building", cli.color_building.as_ref()),
        ("BuildingPart", cli.color_building_part.as_ref()),
        (
            "BuildingInstallation",
            cli.color_building_installation.as_ref(),
        ),
        ("TINRelief", cli.color_tin_relief.as_ref()),
        ("Road", cli.color_road.as_ref()),
        ("Railway", cli.color_railway.as_ref()),
        ("TransportSquare", cli.color_transport_square.as_ref()),
        ("WaterBody", cli.color_water_body.as_ref()),
        ("PlantCover", cli.color_plant_cover.as_ref()),
        (
            "SolitaryVegetationObject",
            cli.color_solitary_vegetation_object.as_ref(),
        ),
        ("LandUse", cli.color_land_use.as_ref()),
        ("CityFurniture", cli.color_city_furniture.as_ref()),
        ("Bridge", cli.color_bridge.as_ref()),
        ("BridgePart", cli.color_bridge_part.as_ref()),
        ("BridgeInstallation", cli.color_bridge_installation.as_ref()),
        (
            "BridgeConstructiveElement",
            cli.color_bridge_construction_element.as_ref(),
        ),
        ("Tunnel", cli.color_tunnel.as_ref()),
        ("TunnelPart", cli.color_tunnel_part.as_ref()),
        ("TunnelInstallation", cli.color_tunnel_installation.as_ref()),
        ("GenericCityObject", cli.color_generic_city_object.as_ref()),
    ] {
        if let Some(color) = color {
            feature_type_colors.insert(feature_type.to_string(), color.clone());
        }
    }

    cityjson_convert::ExportOptions {
        native_glb_color: "#FFC0CB".to_string(),
        metadata_class_name: cli
            .cesium3dtiles_metadata_class
            .clone()
            .unwrap_or_else(|| "cityobject".to_string()),
        feature_type_colors,
        source_crs,
        ecef_origin,
        clip_bbox,
        reproject_to_ecef: true,
        smooth_normals: cli.smooth_normals,
        quantize_geometry: true,
        meshopt_compression: true,
    }
}

fn build_feature_type_lods(cli: &crate::cli::Cli) -> BTreeMap<String, String> {
    let mut feature_type_lods = BTreeMap::new();

    for (feature_type, lod) in [
        ("Building", cli.lod_building.as_ref()),
        ("BuildingPart", cli.lod_building_part.as_ref()),
        (
            "BuildingInstallation",
            cli.lod_building_installation.as_ref(),
        ),
        ("TINRelief", cli.lod_tin_relief.as_ref()),
        ("Road", cli.lod_road.as_ref()),
        ("Railway", cli.lod_railway.as_ref()),
        ("TransportSquare", cli.lod_transport_square.as_ref()),
        ("WaterBody", cli.lod_water_body.as_ref()),
        ("PlantCover", cli.lod_plant_cover.as_ref()),
        (
            "SolitaryVegetationObject",
            cli.lod_solitary_vegetation_object.as_ref(),
        ),
        ("LandUse", cli.lod_land_use.as_ref()),
        ("CityFurniture", cli.lod_city_furniture.as_ref()),
        ("Bridge", cli.lod_bridge.as_ref()),
        ("BridgePart", cli.lod_bridge_part.as_ref()),
        ("BridgeInstallation", cli.lod_bridge_installation.as_ref()),
        (
            "BridgeConstructiveElement",
            cli.lod_bridge_construction_element.as_ref(),
        ),
        ("Tunnel", cli.lod_tunnel.as_ref()),
        ("TunnelPart", cli.lod_tunnel_part.as_ref()),
        ("TunnelInstallation", cli.lod_tunnel_installation.as_ref()),
        ("GenericCityObject", cli.lod_generic_city_object.as_ref()),
    ] {
        if let Some(lod) = lod {
            feature_type_lods.insert(feature_type.to_string(), lod.clone());
        }
    }

    feature_type_lods
}

fn compute_root_ecef_origin(
    world: &parser::World,
    quadtree: &spatial_structs::QuadTree,
) -> Result<[f64; 3], Box<dyn std::error::Error>> {
    let crs_from = format!("EPSG:{}", world.crs.to_epsg()?);
    let transformer = crate::proj::Proj::new_known_crs(&crs_from, "EPSG:4978", None)?;
    let root_bbox = quadtree.bbox(&world.grid);
    let root_center_original = (
        (root_bbox[0] + root_bbox[3]) * 0.5,
        (root_bbox[1] + root_bbox[4]) * 0.5,
        (root_bbox[2] + root_bbox[5]) * 0.5,
    );
    let root_center_ecef = transformer.convert(root_center_original)?;
    Ok([root_center_ecef.0, root_center_ecef.1, root_center_ecef.2])
}

fn prepare_input(
    cli: &crate::cli::Cli,
    output_dir: &Path,
) -> Result<PreparedInput, Box<dyn std::error::Error>> {
    match cityjson_index::resolve_dataset(&cli.input, None) {
        Ok(resolved) => {
            let inspection = resolved.inspect()?;
            let mut city_index =
                cityjson_index::CityIndex::open(resolved.storage_layout(), &resolved.index_path)?;
            if !inspection.index.exists || inspection.index.fresh != Some(true) {
                info!(
                    "Rebuilding cjindex sidecar at {}",
                    resolved.index_path.display()
                );
                city_index.reindex()?;
            }
            let feature_base_document = derive_base_document(&city_index)?;
            let metadata_dir = output_dir.join("metadata");
            fs::create_dir_all(&metadata_dir)?;
            let metadata_path = metadata_dir.join("cjindex-metadata.city.json");
            fs::write(&metadata_path, &feature_base_document)?;
            Ok(PreparedInput {
                source: parser::InputSource::from_cjindex_resolved(&resolved),
                metadata_path,
                feature_base_document: Some(feature_base_document),
            })
        }
        Err(_error) => {
            let metadata_path = cli.input.join("metadata.city.json");
            if !metadata_path.is_file() {
                return Err(format!(
                    "{} is neither a cjindex dataset root nor a legacy dataset root containing metadata.city.json",
                    cli.input.display()
                )
                .into());
            }
            Ok(PreparedInput {
                source: parser::InputSource::LegacyFeatureFiles {
                    features_root: cli.input.clone(),
                },
                metadata_path,
                feature_base_document: None,
            })
        }
    }
}

fn derive_base_document(
    city_index: &cityjson_index::CityIndex,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let metadata = city_index.metadata()?;
    let Some(base_document) = metadata.first() else {
        return Err("cjindex dataset does not contain any source metadata".into());
    };
    if metadata
        .iter()
        .skip(1)
        .any(|candidate| candidate.as_ref() != base_document.as_ref())
    {
        return Err(
            "cjindex dataset contains multiple metadata documents; tyler requires one shared base document".into(),
        );
    }
    Ok(serde_json::to_vec(base_document.as_ref())?)
}

fn collect_tile_feature_ids(
    world: &parser::World,
    qtree_node: &spatial_structs::QuadTree,
) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut feature_ids = Vec::new();
    for cellid in qtree_node.cells() {
        let cell = world.grid.cell(cellid);
        for fid in &cell.feature_ids {
            if seen.insert(*fid) {
                feature_ids.push(*fid);
            }
        }
    }
    feature_ids
}

fn read_tile_feature_models(
    world: &parser::World,
    feature_ids: &[usize],
) -> Result<Vec<cityjson_lib::CityModel>, Box<dyn std::error::Error>> {
    let mut models = Vec::with_capacity(feature_ids.len());
    match &world.input_source {
        parser::InputSource::LegacyFeatureFiles { features_root } => {
            for fid in feature_ids {
                let parser::FeatureReference::LegacyPath(relative_path) =
                    &world.features[*fid].reference
                else {
                    return Err("legacy input unexpectedly referenced a cjindex feature".into());
                };
                let feature_path = features_root.join(relative_path);
                models.push(cityjson_lib::json::staged::from_feature_file_with_base(
                    &feature_path,
                    &world.feature_base_document,
                )?);
            }
        }
        parser::InputSource::CjIndexDataset { .. } => {
            let city_index = world.input_source.open_index()?;
            for fid in feature_ids {
                let parser::FeatureReference::CjIndexId(feature_id) =
                    &world.features[*fid].reference
                else {
                    return Err(
                        "cjindex input unexpectedly referenced a legacy feature path".into(),
                    );
                };
                let model = city_index.get(feature_id)?.ok_or_else(|| {
                    format!("feature {feature_id} could not be resolved from cjindex")
                })?;
                models.push(model);
            }
        }
    }

    Ok(models)
}

fn build_tile_model_from_feature_ids(
    world: &parser::World,
    feature_ids: &[usize],
    feature_type_lods: &BTreeMap<String, String>,
) -> Result<cityjson_lib::CityModel, Box<dyn std::error::Error>> {
    let models = prepare_tile_feature_models(world, feature_ids, feature_type_lods)?;
    if models.is_empty() {
        return Err("tile model preparation removed all CityObjects".into());
    }
    let merged = cityjson_lib::ops::merge(models)?;
    cleanup_and_update_extents(merged)
}

#[cfg(test)]
fn build_tile_model(
    world: &parser::World,
    qtree_node: &spatial_structs::QuadTree,
) -> Result<cityjson_lib::CityModel, Box<dyn std::error::Error>> {
    let feature_ids = collect_tile_feature_ids(world, qtree_node);
    build_tile_model_from_feature_ids(world, &feature_ids, &BTreeMap::new())
}

fn build_tile_debug_cityjsonseq(
    world: &parser::World,
    feature_ids: &[usize],
    feature_type_lods: &BTreeMap<String, String>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let models = prepare_tile_feature_models(world, feature_ids, feature_type_lods)?;
    let mut feature_output = Vec::new();
    for model in models {
        cityjson_lib::json::to_feature_writer(&mut feature_output, &model)?;
        feature_output.write_all(b"\n")?;
    }
    Ok(feature_output)
}

fn prepare_tile_feature_models(
    world: &parser::World,
    feature_ids: &[usize],
    feature_type_lods: &BTreeMap<String, String>,
) -> Result<Vec<cityjson_lib::CityModel>, Box<dyn std::error::Error>> {
    read_tile_feature_models(world, feature_ids)?
        .into_iter()
        .filter_map(
            |model| match prepare_feature_model(model, world, feature_type_lods) {
                Ok(Some(model)) => Some(Ok(model)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect()
}

fn prepare_feature_model(
    model: cityjson_lib::CityModel,
    world: &parser::World,
    feature_type_lods: &BTreeMap<String, String>,
) -> Result<Option<cityjson_lib::CityModel>, Box<dyn std::error::Error>> {
    let mut model = filter_cityobject_types(model, world.cityobject_types.as_ref())?;
    prune_lod_geometries(&mut model, feature_type_lods)?;
    let model = remove_empty_geometry_cityobjects(&model)?;
    if model.cityobjects().is_empty() {
        return Ok(None);
    }
    cleanup_and_update_extents(model).map(Some)
}

fn filter_cityjsonfeature_preserving_root<F>(
    model: &cityjson_lib::CityModel,
    predicate: F,
) -> Result<cityjson_lib::CityModel, Box<dyn std::error::Error>>
where
    F: FnMut(cityjson_lib::ops::FilterContext<'_>) -> bool,
{
    let had_feature_root = model.id().is_some();
    let mut filtered = cityjson_lib::ops::filter(model, predicate)?;

    if !had_feature_root || filtered.id().is_some() || filtered.cityobjects().is_empty() {
        return Ok(filtered);
    }

    let replacement_root = parentless_cityobject_handle(&filtered).ok_or(
        "filtered CityJSONFeature kept CityObjects but has no parentless replacement root",
    )?;
    filtered.set_id(Some(replacement_root));

    Ok(filtered)
}

fn parentless_cityobject_handle(model: &cityjson_lib::CityModel) -> Option<CityObjectHandle> {
    model.cityobjects().iter().find_map(|(handle, cityobject)| {
        let has_surviving_parent = cityobject.parents().is_some_and(|parents| {
            parents
                .iter()
                .any(|parent| model.cityobjects().get(*parent).is_some())
        });
        (!has_surviving_parent).then_some(handle)
    })
}

fn filter_cityobject_types(
    model: cityjson_lib::CityModel,
    cityobject_types: Option<&Vec<parser::CityObjectType>>,
) -> Result<cityjson_lib::CityModel, Box<dyn std::error::Error>> {
    let Some(cityobject_types) = cityobject_types else {
        return Ok(model);
    };
    let selected = cityobject_types
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<HashSet<_>>();
    filter_cityjsonfeature_preserving_root(&model, |ctx| {
        selected.contains(&ctx.cityobject().type_cityobject().to_string())
    })
}

fn prune_lod_geometries(
    model: &mut cityjson_lib::CityModel,
    feature_type_lods: &BTreeMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if feature_type_lods.is_empty() {
        return Ok(());
    }

    let retained_by_object = model
        .cityobjects()
        .iter()
        .map(|(handle, cityobject)| {
            let feature_type = cityobject.type_cityobject().to_string();
            let retained = cityobject
                .geometry()
                .unwrap_or(&[])
                .iter()
                .copied()
                .filter(|geometry_handle| {
                    geometry_matches_lod(
                        model,
                        *geometry_handle,
                        feature_type_lods.get(&feature_type),
                    )
                })
                .collect::<Vec<_>>();
            (handle, retained)
        })
        .collect::<Vec<_>>();

    for (handle, retained) in retained_by_object {
        let cityobject = model
            .cityobjects_mut()
            .get_mut(handle)
            .ok_or_else(|| format!("missing CityObject handle {handle} during LoD pruning"))?;
        cityobject.clear_geometry();
        for geometry_handle in retained {
            cityobject.add_geometry(geometry_handle);
        }
    }

    Ok(())
}

fn geometry_matches_lod(
    model: &cityjson_lib::CityModel,
    geometry_handle: GeometryHandle,
    selected_lod: Option<&String>,
) -> bool {
    let Some(selected_lod) = selected_lod else {
        return true;
    };
    model
        .get_geometry(geometry_handle)
        .and_then(|geometry| geometry.lod())
        .is_some_and(|lod| lod.to_string() == *selected_lod)
}

fn remove_empty_geometry_cityobjects(
    model: &cityjson_lib::CityModel,
) -> Result<cityjson_lib::CityModel, Box<dyn std::error::Error>> {
    filter_cityjsonfeature_preserving_root(model, |ctx| {
        ctx.cityobject()
            .geometry()
            .is_some_and(|geometries| !geometries.is_empty())
    })
}

fn cleanup_and_update_extents(
    model: cityjson_lib::CityModel,
) -> Result<cityjson_lib::CityModel, Box<dyn std::error::Error>> {
    let mut model = cityjson_lib::ops::cleanup(&model)?;
    let handles = model.cityobjects().ids().collect::<Vec<_>>();
    for handle in handles {
        let extent = model.calculate_cityobject_geographical_extent(handle)?;
        let cityobject = model
            .cityobjects_mut()
            .get_mut(handle)
            .ok_or_else(|| format!("missing CityObject handle {handle} during extent update"))?;
        cityobject.set_geographical_extent(extent);
    }
    if let Some(extent) = model.calculate_geographical_extent()? {
        model.metadata_mut().set_geographical_extent(extent);
    }
    Ok(model)
}

fn write_debug_tile_input(
    path_features_input_dir: &Path,
    file_name: &str,
    cityjsonseq_bytes: &[u8],
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    fs::create_dir_all(path_features_input_dir)?;
    let path_tile_ndjson = path_features_input_dir
        .join(file_name)
        .with_extension("city.jsonl");
    if let Some(parent) = path_tile_ndjson.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path_tile_ndjson, cityjsonseq_bytes)?;
    Ok(path_tile_ndjson)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // --- Begin argument parsing
    let cli = crate::cli::Cli::parse();
    debug!("{:?}", &cli);
    info!("tyler version: {}", clap::crate_version!());
    if !cli.output.is_dir() {
        fs::create_dir_all(&cli.output)?;
        info!("Created output directory {:#?}", &cli.output);
    }
    // Since we have a default value, we can safely unwrap.
    let grid_cellsize = cli.grid_cellsize.unwrap();
    let geometric_error_above_leaf = cli.geometric_error_above_leaf.unwrap();
    let format = Formats::_3DTiles; // override --format
                                    // Since we have a default value, it is safe to unwrap
                                    // let qtree_capacity = 0; // override cli.qtree_capacity
    let qtree_criteria = spatial_structs::QuadTreeCriteria::Vertices; // override --qtree-criteria
    let quadtree_capacity = match qtree_criteria {
        spatial_structs::QuadTreeCriteria::Objects => {
            spatial_structs::QuadTreeCapacity::Objects(cli.qtree_capacity.unwrap())
        }
        spatial_structs::QuadTreeCriteria::Vertices => {
            spatial_structs::QuadTreeCapacity::Vertices(cli.qtree_capacity.unwrap())
        }
    };
    #[allow(unused)]
    let metadata_class: String = match format {
        Formats::_3DTiles => {
            if cli.cesium3dtiles_tileset_only {
                String::new()
            } else if cli.cesium3dtiles_metadata_class.is_none() {
                panic!("metadata_class must be set for writing 3D Tiles")
            } else {
                cli.cesium3dtiles_metadata_class.clone().unwrap()
            }
        }
        Formats::CityJSON => "".to_string(),
    };
    if cli.cesium3dtiles_content_bv_from_tile && !cli.cesium3dtiles_content_add_bv {
        warn!("cesium3dtiles_content_bv_from_tile is true, but cesium3dtiles_content_add_bv is false. The tile content bounding volumes are not going to be added, unless you set --3dtiles-content-add-bv");
    }
    let debug_data = match cli.debug_load_data {
        None => DebugData::default(),
        Some(ref dir_path) => {
            if dir_path.is_dir() {
                let world_path = dir_path.join("world.bincode");
                let quadtree_path = dir_path.join("quadtree.bincode");
                let _tileset_path = dir_path.join("tileset.bincode");
                let tiles_results_path = dir_path.join("tiles_results.bincode");
                DebugData {
                    world: world_path.exists().then_some(world_path),
                    quadtree: quadtree_path.exists().then_some(quadtree_path),
                    tiles_results: tiles_results_path.exists().then_some(tiles_results_path),
                }
            } else {
                warn!(
                    "debug_load_data {dir_path:?} is not a directory, cannot load .bincode files"
                );
                DebugData::default()
            }
        }
    };
    debug!("{:?}", debug_data);
    let debug_data_output_path = cli.output.join("debug");
    if (cli.grid_export || log_enabled!(Level::Debug)) && !debug_data_output_path.exists() {
        fs::create_dir(&debug_data_output_path)?;
    }
    // --- end of argument parsing

    // Populate the World with features
    // Primitive types that implement Copy are efficiently copied into the function and
    // and it is cleaner to avoid the indirection. However, heap-allocated container
    // types are best passed by reference, because it is "expensive" to Clone them
    // (they don't implement Copy). When we move a value, we explicitly transfer
    // ownership of the value (eg cli.object_type).
    let prepared_input = if debug_data.world.is_none() {
        Some(prepare_input(&cli, &cli.output)?)
    } else {
        None
    };
    let cityobject_types = cli.object_type.clone();

    let world: parser::World = match debug_data.world {
        None => {
            let prepared_input = prepared_input
                .as_ref()
                .expect("prepared input must exist when world is built from source");
            let mut world = match &prepared_input.feature_base_document {
                Some(feature_base_document) => parser::World::from_cjindex(
                    prepared_input.source.clone(),
                    prepared_input.metadata_path.clone(),
                    feature_base_document.clone(),
                    grid_cellsize,
                    cityobject_types,
                    cli.grid_minz,
                    cli.grid_maxz,
                )?,
                None => parser::World::new(
                    &prepared_input.metadata_path,
                    &cli.input,
                    grid_cellsize,
                    cityobject_types,
                    cli.grid_minz,
                    cli.grid_maxz,
                )?,
            };
            world.index_with_grid()?; // todo input: in general, build a line index
            world
        }
        Some(world_path) => {
            info!("Loading world from bincode {world_path:?}");
            let world_file = File::open(world_path)?;
            bincode::deserialize_from(world_file)?
        }
    };

    info!(
        "Computed grid statistics: {}",
        world.grid.compute_statistics()
    );

    if cli.grid_export {
        info!("Exporting the grid to TSV to {:?}", &debug_data_output_path);
        world.export_grid(cli.grid_export_features, Some(&debug_data_output_path))?;
    }
    if log_enabled!(Level::Debug) {
        debug!(
            "Exporting the world instance to bincode to {:?}",
            &debug_data_output_path
        );
        world.export_bincode(Some("world"), Some(&debug_data_output_path))?;
    }

    // Build quadtree
    let quadtree: spatial_structs::QuadTree = match debug_data.quadtree {
        None => {
            info!("Building quadtree");
            spatial_structs::QuadTree::from_world(&world, quadtree_capacity)
        }
        Some(quadtree_path) => {
            info!("Loading quadtree from bincode {quadtree_path:?}");
            let quadtree_file = File::open(quadtree_path)?;
            bincode::deserialize_from(quadtree_file)?
        }
    };

    if cli.grid_export {
        info!(
            "Exporting the quadtree to TSV to {:?}",
            &debug_data_output_path
        );
        quadtree.export(&world, Some(&debug_data_output_path))?;
    }
    if log_enabled!(Level::Debug) {
        debug!(
            "Exporting the quadtree instance to bincode to {:?}",
            &debug_data_output_path
        );
        quadtree.export_bincode(Some("quadtree"), Some(&debug_data_output_path))?;
    }

    // 3D Tiles

    let tileset_path = cli.output.join("tileset.json");
    let subtrees_path = cli.output.join("subtrees");
    let tileset_path_unpruned = cli.output.join("tileset_unpruned.json");
    let subtrees_path_unpruned = cli.output.join("subtrees_unpruned");
    info!("Generating 3D Tiles tileset");
    let mut tileset = formats::cesium3dtiles::Tileset::from_quadtree(
        &quadtree,
        &world,
        geometric_error_above_leaf,
        grid_cellsize,
        cli.grid_minz,
        cli.grid_maxz,
        cli.cesium3dtiles_content_bv_from_tile,
        cli.cesium3dtiles_content_add_bv,
    );

    if cli.grid_export {
        info!(
            "Exporting the explicit tileset to TSV files to {:?}",
            &debug_data_output_path
        );
        tileset.export(Some(&debug_data_output_path))?;
    }

    let (tiles, _subtrees) = match cli.cesium3dtiles_implicit {
        true => {
            let mut tileset_implicit = tileset.clone();
            // FIXME: here we have a Vec<(Tile, TileId)> in 'tiles' instead of Vec<&Tile>, because of the
            //  mess with the implicit/explicit tile id-s.
            info!("Converting to implicit tiling");
            // Tileset.make_implicit() outputs the tiles that have content. If only the leaves have
            //  content, then only the leaves are outputted.
            let components: Vec<_> = subtrees_path_unpruned
                .components()
                .map(|comp| comp.as_os_str())
                .collect();
            let subtrees_dir_option = components.last().cloned().unwrap().to_str();
            let tiles_subtrees = tileset_implicit.make_implicit(
                &world.grid,
                &quadtree,
                cli.grid_export,
                subtrees_dir_option,
                Some(&debug_data_output_path),
            );

            if cli.cesium3dtiles_tileset_only || log_enabled!(Level::Debug) {
                info!("Writing unpruned 3D Tiles tileset");
                tileset_implicit.to_file(&tileset_path_unpruned)?;

                info!("Writing unpruned subtrees for implicit tiling");
                fs::create_dir_all(&subtrees_path_unpruned)?;
                for (subtree_id, subtree_bytes) in &tiles_subtrees.1 {
                    fs::create_dir_all(
                        subtrees_path_unpruned
                            .join(format!("{}/{}", subtree_id.level, subtree_id.x)),
                    )
                    .unwrap();
                    let out_path = subtrees_path_unpruned
                        .join(&subtree_id.to_string())
                        .with_extension("subtree");
                    let mut subtree_file = File::create(&out_path)
                        .unwrap_or_else(|_| panic!("could not create {:?} for writing", &out_path));
                    if let Err(_e) = subtree_file.write_all(subtree_bytes) {
                        warn!("Failed to write subtree {} content", subtree_id);
                    }
                }
            }

            tiles_subtrees
        }
        false => {
            let just_tiles = tileset.collect_leaves();
            // FIXME: here we need Vec<(Tile, TileId)> instead of Vec<&Tile>, for the same reason
            //  as above
            let tiles: Vec<(Tile, TileId)> = just_tiles
                .into_iter()
                .map(|tile_ref| (tile_ref.clone(), tile_ref.id.clone()))
                .collect();

            info!("Writing unpruned 3D Tiles tileset");
            tileset.to_file(&tileset_path_unpruned)?;

            (tiles, vec![])
        }
    };

    // Export each tile by merging its selected CityJSONFeature stream in memory.
    let path_output_tiles = cli.output.join("t");
    let path_features_input_dir = cli.output.join("inputs");
    // TODO: need to refactor this parallel loop somehow that it does not only read the
    //  3d tiles tiles, but also works with cityjson output
    if !cli.cesium3dtiles_tileset_only {
        fs::create_dir_all(&path_output_tiles)?;
        info!("Created output directory {:#?}", &path_output_tiles);
        if cli.debug_tile_inputs {
            fs::create_dir_all(&path_features_input_dir)?;
            info!("Created output directory {:#?}", &path_features_input_dir);
        }

        let source_crs = Some(format!("EPSG:{}", world.crs.to_epsg()?));
        let ecef_origin = Some(compute_root_ecef_origin(&world, &quadtree)?);
        let export_options = build_glb_export_options(&cli, source_crs, ecef_origin, None);
        let feature_type_lods = build_feature_type_lods(&cli);
        let tiles_len = tiles.len();
        let tiles_failed_iter = tiles.into_par_iter().map(|(tile, tileid)| {
            let tileid_grid = &tile.id;
            let qtree_nodeid: spatial_structs::QuadTreeNodeId = tileid_grid.into();
            let qtree_node = quadtree
                .node(&qtree_nodeid)
                .unwrap_or_else(|| panic!("did not find tile {} in quadtree", tileid_grid));
            if qtree_node.nr_items == 0 {
                // The Tileset.prune() method removes the empty tiles from the tileset,
                //  so skipping the tile conversion without failure is ok if it's empty.
                debug!("Tile is empty ({}), skipping conversion", tileid_grid);
                return None;
            }
            let tileid_string = tileid.to_string();
            let file_name = tileid_string;
            let output_file = path_output_tiles.join(&file_name).with_extension("glb");
            let feature_ids = collect_tile_feature_ids(&world, qtree_node);
            let model =
                match build_tile_model_from_feature_ids(&world, &feature_ids, &feature_type_lods) {
                    Ok(model) => model,
                    Err(error) => {
                        warn!(
                            "Failed to build CityJSON model for tile {}: {}",
                            tileid_grid, error
                        );
                        return Some(tile);
                    }
                };
            if cli.debug_tile_inputs {
                let cityjsonseq_bytes =
                    match build_tile_debug_cityjsonseq(&world, &feature_ids, &feature_type_lods) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            warn!(
                                "Failed to build debug CityJSONFeature stream for tile {}: {}",
                                tileid_grid, error
                            );
                            return Some(tile);
                        }
                    };
                if let Err(error) = write_debug_tile_input(
                    &path_features_input_dir,
                    file_name.as_str(),
                    &cityjsonseq_bytes,
                ) {
                    warn!(
                        "Failed to write debug CityJSONFeature stream for tile {}: {}",
                        tileid_grid, error
                    );
                    return Some(tile);
                }
            }
            let mut tile_export_options = export_options.clone();
            if cli.cesium3dtiles_content_clip_to_tile_bounds {
                tile_export_options.clip_bbox = Some(qtree_node.bbox(&world.grid));
            }
            if let Err(error) =
                cityjson_convert::convert_to_glb(&model, &output_file, &tile_export_options)
            {
                warn!("Tile {} conversion failed: {}", tileid_grid, error);
                return Some(tile);
            }
            if !output_file.exists() {
                warn!(
                    "Tile {} conversion failed: {} was not created",
                    tileid_grid,
                    output_file.display()
                );
                return Some(tile);
            }

            None
        });

        let mut tiles_results: Vec<Option<Tile>> = Vec::with_capacity(tiles_len + 2);
        if let Some(tiles_results_path) = debug_data.tiles_results {
            info!("Loading tiles_results from {tiles_results_path:?}");
            let tiles_results_file = File::open(tiles_results_path)?;
            tiles_results = bincode::deserialize_from(tiles_results_file)?
        } else {
            info!("Converting and optimizing {tiles_len} tiles");
            tiles_failed_iter.collect_into_vec(&mut tiles_results);
            if log_enabled!(Level::Debug) {
                debug!(
                    "Exporting the tiles_results instance to bincode to {:?}",
                    &debug_data_output_path
                );
                let outpath = debug_data_output_path.join("tiles_results.bincode");
                let tiles_results_file = File::create(outpath)?;
                bincode::serialize_into(tiles_results_file, &tiles_results)?;
            }
        }
        let tiles_failed: Vec<Tile> = tiles_results.into_iter().flatten().collect();
        info!("Done");

        info!("Pruning tileset of {} failed tiles", tiles_failed.len());
        for (i, failed) in tiles_failed.iter().enumerate() {
            debug!("{}, removing failed from the tileset: {}", i, failed.id);
        }
        // Remove tiles that failed the gltf conversion
        tileset.prune(&tiles_failed, &quadtree);
        if cli.cesium3dtiles_implicit {
            // FIXME: here we re-create the implicit tileset from the pruned tileset,
            //  because it is simpler than flipping the bits of the unavailable tiles,
            //  because of the mixed up explicit/implicit tile IDs. But ideally, we
            //  flip the bits, so we won't need to duplicate the tileset here.
            let components: Vec<_> = subtrees_path
                .components()
                .map(|comp| comp.as_os_str())
                .collect();
            let subtrees_dir_option = components.last().cloned().unwrap().to_str();
            let (_, subtrees) = tileset.make_implicit(
                &world.grid,
                &quadtree,
                cli.grid_export,
                subtrees_dir_option,
                Some(&debug_data_output_path),
            );
            info!("Writing subtrees for implicit tiling");
            fs::create_dir_all(&subtrees_path)?;
            for (subtree_id, subtree_bytes) in subtrees {
                fs::create_dir_all(
                    subtrees_path.join(format!("{}/{}", subtree_id.level, subtree_id.x)),
                )
                .unwrap();
                let out_path = subtrees_path
                    .join(&subtree_id.to_string())
                    .with_extension("subtree");
                let mut subtree_file = File::create(&out_path)
                    .unwrap_or_else(|_| panic!("could not create {:?} for writing", &out_path));
                if let Err(_e) = subtree_file.write_all(&subtree_bytes) {
                    warn!("Failed to write subtree {} content", subtree_id);
                }
            }
        } else {
            let available_levels = tileset.available_levels();
            // A five level deep tree is still managable in size.
            if available_levels > 5 {
                // Try to find the split where each child tileset starts to have more tiles in their
                // tree, than the ancestor tree. This way, the main tileset is smaller in size than
                // the child tilesets, so it loads faster. This method is not very accurate, because
                // it doesn't account for the actual number of tiles on each level, it only
                // calculates with the theoretical maximum.
                let mut split_at_level = 0;
                for level in (0..available_levels).rev() {
                    let subtree_depth: u32 = (available_levels - level) as u32;
                    let nr_tiles_subtree = (4_usize.pow(subtree_depth) - 1) / 3;
                    let ancestor_tree_depth: u32 =
                        (available_levels - (available_levels - level)) as u32;
                    let nr_tiles_ancestor = (4_usize.pow(ancestor_tree_depth) - 1) / 3;
                    if nr_tiles_ancestor < nr_tiles_subtree {
                        split_at_level = level;
                        break;
                    }
                }
                info!(
                    "Splitting the explicit tileset into external tilesets at level {}",
                    split_at_level
                );
                let external_tilesets = tileset.split(split_at_level);
                for (filename, child_tileset) in &external_tilesets {
                    let tileset_path = cli.output.join(filename);
                    child_tileset.to_file(&tileset_path)?;
                }
            }
        }
        info!("Writing 3D Tiles tileset");
        tileset.to_file(&tileset_path)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("tyler-{prefix}-{unique}"));
        fs::create_dir_all(&path).expect("create test dir");
        path
    }

    fn resource_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("data")
            .join(name)
    }

    fn build_quadtree(world: &parser::World) -> spatial_structs::QuadTree {
        spatial_structs::QuadTree::from_world(world, spatial_structs::QuadTreeCapacity::Objects(1))
    }

    fn feature_root_id(model: &cityjson_lib::CityModel) -> Option<String> {
        model.id().and_then(|handle| {
            model
                .cityobjects()
                .get(handle)
                .map(|cityobject| cityobject.id().to_owned())
        })
    }

    fn feature_root_repair_fixture() -> cityjson_lib::CityModel {
        cityjson_lib::json::from_feature_slice(
            br#"{
                "type":"CityJSONFeature",
                "id":"root-building",
                "CityObjects":{
                    "root-building":{"type":"Building","children":["building-part-1"]},
                    "building-part-1":{"type":"BuildingPart","parents":["root-building"]},
                    "other-building":{"type":"Building"}
                },
                "vertices":[]
            }"#,
        )
        .expect("feature root repair fixture should parse")
    }

    #[test]
    fn feature_root_hotfix_keeps_surviving_root() {
        let model = feature_root_repair_fixture();

        let filtered =
            filter_cityjsonfeature_preserving_root(&model, |ctx| ctx.id() == "root-building")
                .expect("root-preserving filter should succeed");

        assert_eq!(
            feature_root_id(&filtered),
            Some("root-building".to_string())
        );
    }

    #[test]
    fn feature_root_hotfix_reroots_to_parentless_survivor() {
        let model = feature_root_repair_fixture();

        let filtered =
            filter_cityjsonfeature_preserving_root(&model, |ctx| ctx.id() == "other-building")
                .expect("root-repairing filter should succeed");

        assert_eq!(
            feature_root_id(&filtered),
            Some("other-building".to_string())
        );

        let mut feature_output = Vec::new();
        cityjson_lib::json::to_feature_writer(&mut feature_output, &filtered)
            .expect("repaired feature should serialize");
        let feature: Value =
            serde_json::from_slice(&feature_output).expect("serialized feature should parse");

        assert_eq!(
            feature.get("id").and_then(Value::as_str),
            Some("other-building")
        );
    }

    #[test]
    fn feature_root_hotfix_allows_empty_filtered_feature() {
        let model = feature_root_repair_fixture();

        let filtered = filter_cityjsonfeature_preserving_root(&model, |_| false)
            .expect("empty feature filter should not fail");

        assert!(filtered.cityobjects().is_empty());
        assert_eq!(feature_root_id(&filtered), None);
    }

    #[test]
    fn prepare_model_filters_cityobject_types_and_updates_extent() {
        let model = cityjson_lib::json::merge_feature_stream_slice(include_bytes!(
            "../cityjson-convert/tests/data/multi_feature_types.city.jsonl"
        ))
        .expect("fixture feature stream should parse");
        let filtered =
            filter_cityobject_types(model, Some(&vec![parser::CityObjectType::Building]))
                .expect("type filter should succeed");
        let filtered = cleanup_and_update_extents(filtered).expect("cleanup should succeed");

        let cityobject_types = filtered
            .cityobjects()
            .iter()
            .map(|(_, cityobject)| cityobject.type_cityobject().to_string())
            .collect::<Vec<_>>();
        assert_eq!(cityobject_types, vec!["Building"]);
        assert_eq!(
            filtered
                .metadata()
                .and_then(|metadata| metadata.geographical_extent())
                .copied(),
            filtered
                .calculate_geographical_extent()
                .expect("extent calculation should succeed")
        );
    }

    #[test]
    fn prepare_model_prunes_lod_geometry_before_gltf_writer() {
        let mut model = cityjson_lib::json::merge_feature_stream_slice(include_bytes!(
            "../cityjson-convert/tests/data/multi_lod_building_part.city.jsonl"
        ))
        .expect("fixture feature stream should parse");
        let lods = BTreeMap::from([("BuildingPart".to_string(), "2.2".to_string())]);

        prune_lod_geometries(&mut model, &lods).expect("LoD pruning should succeed");
        let model =
            remove_empty_geometry_cityobjects(&model).expect("empty object removal should succeed");
        let model = cleanup_and_update_extents(model).expect("cleanup should succeed");

        let retained_lods = model
            .cityobjects()
            .iter()
            .flat_map(|(_, cityobject)| cityobject.geometry().unwrap_or(&[]))
            .map(|geometry_handle| {
                model
                    .get_geometry(*geometry_handle)
                    .and_then(|geometry| geometry.lod())
                    .map(std::string::ToString::to_string)
            })
            .collect::<Vec<_>>();
        assert_eq!(retained_lods, vec![Some("2.2".to_string())]);
        assert_eq!(
            model.geometry_count(),
            1,
            "cleanup should remove geometries no longer referenced by CityObjects"
        );
    }

    #[test]
    fn build_tile_model_exports_legacy_features() {
        let dataset_dir = unique_test_dir("legacy");
        let features_dir = dataset_dir.join("features");
        fs::create_dir_all(&features_dir).expect("create features dir");
        let metadata_path = dataset_dir.join("metadata.city.json");
        let feature_path = features_dir.join("sample.city.jsonl");
        fs::copy(resource_path("3dbag_x00.city.json"), &metadata_path).expect("copy metadata");
        fs::copy(resource_path("3dbag_feature_x71.city.jsonl"), &feature_path)
            .expect("copy feature");

        let mut world = parser::World::new(
            &metadata_path,
            &features_dir,
            200,
            Some(vec![parser::CityObjectType::Building]),
            None,
            None,
        )
        .expect("build legacy world");
        world.index_with_grid().expect("index legacy world");
        let quadtree = build_quadtree(&world);
        let model = build_tile_model(&world, &quadtree).expect("build tile model");
        let feature_ids = collect_tile_feature_ids(&world, &quadtree);
        let ndjson = String::from_utf8(
            build_tile_debug_cityjsonseq(&world, &feature_ids, &BTreeMap::new())
                .expect("build debug cityjsonseq"),
        )
        .expect("debug cityjsonseq utf8");

        assert!(!model.cityobjects().is_empty());
        assert!(ndjson.contains("\"type\":\"CityJSONFeature\""));
        assert_eq!(ndjson.lines().count(), 1);
    }

    #[test]
    fn write_debug_tile_input_writes_cityjsonl() {
        let dataset_dir = unique_test_dir("debug-tile-input");
        let inputs_dir = dataset_dir.join("inputs");
        let path = write_debug_tile_input(&inputs_dir, "tile", b"{\"type\":\"CityJSONFeature\"}\n")
            .expect("write debug tile input");

        assert_eq!(path, inputs_dir.join("tile.city.jsonl"));
        assert_eq!(
            fs::read(&path).expect("read debug tile input"),
            b"{\"type\":\"CityJSONFeature\"}\n"
        );
        assert!(!inputs_dir.join("tile.input").exists());

        let nested_path =
            write_debug_tile_input(&inputs_dir, "1/2/3", b"{\"type\":\"CityJSONFeature\"}\n")
                .expect("write nested debug tile input");
        assert_eq!(nested_path, inputs_dir.join("1/2/3.city.jsonl"));
        assert!(nested_path.exists());
    }

    #[test]
    fn build_tile_model_exports_cjindex_ndjson_directly() {
        let dataset_dir = unique_test_dir("cjindex-ndjson");
        let metadata =
            fs::read_to_string(resource_path("3dbag_x00.city.json")).expect("read metadata");
        let feature = fs::read_to_string(resource_path("3dbag_feature_x71.city.jsonl"))
            .expect("read feature");
        let ndjson_source = dataset_dir.join("source.city.jsonl");
        fs::write(&ndjson_source, format!("{metadata}\n{feature}\n")).expect("write ndjson source");

        let resolved =
            cityjson_index::resolve_dataset(&dataset_dir, None).expect("resolve ndjson dataset");
        let mut city_index =
            cityjson_index::CityIndex::open(resolved.storage_layout(), &resolved.index_path)
                .expect("open index");
        city_index.reindex().expect("reindex ndjson dataset");
        let indexed_bounds = city_index
            .iter_all_bbox_pages(1)
            .expect("build bbox page iterator")
            .next()
            .expect("bbox page should exist")
            .expect("bbox page should load")
            .into_iter()
            .next()
            .expect("indexed feature should exist")
            .bounds;
        let feature_base_document = derive_base_document(&city_index).expect("derive base doc");
        let metadata_path = dataset_dir.join("metadata.city.json");
        fs::write(&metadata_path, &feature_base_document).expect("write metadata");

        let mut world = parser::World::from_cjindex(
            parser::InputSource::from_cjindex_resolved(&resolved),
            metadata_path,
            feature_base_document,
            200,
            None,
            None,
            None,
        )
        .expect("build cjindex ndjson world");
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(world.grid.bbox[2], indexed_bounds.min_z);
            assert_eq!(world.grid.bbox[5], indexed_bounds.max_z);
        }
        world.index_with_grid().expect("index cjindex ndjson world");
        let quadtree = build_quadtree(&world);
        let model = build_tile_model(&world, &quadtree).expect("build tile model");

        assert!(!model.cityobjects().is_empty());
        assert!(!model.vertices().is_empty());
    }

    #[test]
    fn build_tile_model_exports_cjindex_ndjson_without_type_filter_directly() {
        let dataset_dir = unique_test_dir("cjindex-ndjson-unfiltered");
        let metadata =
            fs::read_to_string(resource_path("3dbag_x00.city.json")).expect("read metadata");
        let feature = fs::read_to_string(resource_path("3dbag_feature_x71.city.jsonl"))
            .expect("read feature");
        let ndjson_source = dataset_dir.join("source.city.jsonl");
        fs::write(&ndjson_source, format!("{metadata}\n{feature}\n")).expect("write ndjson source");

        let resolved =
            cityjson_index::resolve_dataset(&dataset_dir, None).expect("resolve ndjson dataset");
        let mut city_index =
            cityjson_index::CityIndex::open(resolved.storage_layout(), &resolved.index_path)
                .expect("open index");
        city_index.reindex().expect("reindex ndjson dataset");
        let feature_base_document = derive_base_document(&city_index).expect("derive base doc");
        let metadata_path = dataset_dir.join("metadata.city.json");
        fs::write(&metadata_path, &feature_base_document).expect("write metadata");

        let mut world = parser::World::from_cjindex(
            parser::InputSource::from_cjindex_resolved(&resolved),
            metadata_path,
            feature_base_document,
            200,
            None,
            None,
            None,
        )
        .expect("build cjindex ndjson world");
        world.index_with_grid().expect("index cjindex ndjson world");
        let quadtree = build_quadtree(&world);
        let model = build_tile_model(&world, &quadtree).expect("build tile model");

        assert!(!model.cityobjects().is_empty());
        assert!(!model.vertices().is_empty());
    }

    #[test]
    fn build_tile_model_exports_cjindex_cityjson_directly() {
        let dataset_dir = unique_test_dir("cjindex-cityjson");
        let metadata: Value = serde_json::from_slice(
            &fs::read(resource_path("3dbag_x00.city.json")).expect("read metadata"),
        )
        .expect("parse metadata");
        let feature: Value = serde_json::from_slice(
            &fs::read(resource_path("3dbag_feature_x71.city.jsonl")).expect("read feature"),
        )
        .expect("parse feature");
        let mut cityjson = metadata;
        cityjson["CityObjects"] = feature["CityObjects"].clone();
        cityjson["vertices"] = feature["vertices"].clone();
        let cityjson_path = dataset_dir.join("source.city.json");
        fs::write(
            &cityjson_path,
            serde_json::to_vec(&cityjson).expect("serialize cityjson"),
        )
        .expect("write cityjson source");

        let resolved =
            cityjson_index::resolve_dataset(&dataset_dir, None).expect("resolve cityjson dataset");
        let mut city_index =
            cityjson_index::CityIndex::open(resolved.storage_layout(), &resolved.index_path)
                .expect("open index");
        city_index.reindex().expect("reindex cityjson dataset");
        let feature_base_document = derive_base_document(&city_index).expect("derive base doc");
        let metadata_path = dataset_dir.join("metadata.city.json");
        fs::write(&metadata_path, &feature_base_document).expect("write metadata");

        let mut world = parser::World::from_cjindex(
            parser::InputSource::from_cjindex_resolved(&resolved),
            metadata_path,
            feature_base_document,
            200,
            Some(vec![parser::CityObjectType::Building]),
            None,
            None,
        )
        .expect("build cjindex cityjson world");
        world
            .index_with_grid()
            .expect("index cjindex cityjson world");
        let quadtree = build_quadtree(&world);
        let model = build_tile_model(&world, &quadtree).expect("build tile model");

        assert!(!model.cityobjects().is_empty());
        assert!(!model.vertices().is_empty());
    }
}
