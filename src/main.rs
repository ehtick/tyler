mod cli;
mod formats;
mod parser;
mod proj;
mod spatial_structs;

use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use clap::Parser;
use log::{debug, error, info, log_enabled, Level};
use rayon::prelude::*;
use subprocess::{Exec, Redirection};

#[derive(Debug, Default, Clone)]
struct SubprocessConfig {
    output_extension: String,
    exe: PathBuf,
    script: PathBuf,
}

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // --- Begin argument parsing
    let cli = crate::cli::Cli::parse();
    if !cli.output.is_dir() {
        fs::create_dir_all(&cli.output)?;
        info!("Created output directory {:#?}", &cli.output);
    }
    // Since we have a default value, we can safely unwrap.
    let grid_cellsize = cli.grid_cellsize.unwrap();
    let subprocess_config = match cli.format {
        Formats::_3DTiles => {
            if let Some(exe) = cli.exe_geof {
                SubprocessConfig {
                    output_extension: "glb".to_string(),
                    exe,
                    script: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("resources")
                        .join("geof")
                        .join("createGLB.json"),
                }
            } else {
                panic!("exe_geof must be set for generating 3D Tiles");
            }
        }
        Formats::CityJSON => {
            // TODO: refactor parallel loop
            panic!("cityjson output is not supported");
            // if let Some(exe) = cli.exe_python {
            //     SubprocessConfig {
            //         output_extension: "city.json".to_string(),
            //         exe,
            //         script: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            //             .join("resources")
            //             .join("python")
            //             .join("convert_cityjsonfeatures.py"),
            //     }
            // } else {
            //     panic!("exe_python must be set for generating CityJSON tiles")
            // }
        }
    };
    debug!("{:?}", &subprocess_config);
    // Since we have a default value, it is safe to unwrap
    let quadtree_capacity = match &cli.qtree_criteria.unwrap() {
        spatial_structs::QuadTreeCriteria::Objects => {
            spatial_structs::QuadTreeCapacity::Objects(cli.qtree_capacity.unwrap())
        }
        spatial_structs::QuadTreeCriteria::Vertices => {
            spatial_structs::QuadTreeCapacity::Vertices(cli.qtree_capacity.unwrap())
        }
    };
    let metadata_class: String = match cli.format {
        Formats::_3DTiles => {
            if cli.metadata_class.is_none() {
                panic!("metadata_class must be set for writing 3D Tiles")
            } else {
                cli.metadata_class.unwrap()
            }
        }
        Formats::CityJSON => "".to_string(),
    };
    // --- end of argument parsing

    // Populate the World with features
    // Primitive types that implement Copy are efficiently copied into the function and
    // and it is cleaner to avoid the indirection. However, heap-allocated container
    // types are best passed by reference, because it is "expensive" to Clone them
    // (they don't implement Copy). When we move a value, we explicitly transfer
    // ownership of the value (eg cli.object_type).
    let mut world = parser::World::new(
        &cli.metadata,
        &cli.features,
        grid_cellsize,
        cli.object_type,
        cli.grid_minz,
        cli.grid_maxz,
    )?;
    world.index_with_grid();

    // Debug
    if cli.grid_export {
        debug!("Exporting the grid to the working directory");
        world.export_grid()?;
    }

    // Build quadtree
    info!("Building quadtree");
    let quadtree = spatial_structs::QuadTree::from_world(&world, quadtree_capacity);

    // Debug
    if cli.grid_export {
        debug!("Exporting the quadtree to the working directory");
        quadtree.export(&world.grid)?;
    }

    // let tiles: Vec<&formats::cesium3dtiles::Tile> = Vec::new();
    // if cli.format == Formats::_3DTiles {
    //     // 3D Tiles
    //     info!("Generating 3D Tiles tileset");
    //     let tileset_path = cli.output.join("tileset.json");
    //     let tileset = formats::cesium3dtiles::Tileset::from_quadtree(
    //         &quadtree,
    //         &world,
    //         cli.grid_minz,
    //         cli.grid_maxz,
    //     );
    //     tileset.to_file(tileset_path)?;
    //     tiles = tileset.flatten(Some(4));
    // }
    // 3D Tiles
    info!("Generating 3D Tiles tileset");
    let tileset_path = cli.output.join("tileset.json");
    let mut tileset = formats::cesium3dtiles::Tileset::from_quadtree(
        &quadtree,
        &world,
        cli.grid_minz,
        cli.grid_maxz,
    );
    // Select how many levels of tiles from the hierarchy do we want to export with
    // content.
    tileset.add_content(cli.qtree_export_levels);
    let tiles = tileset.flatten(cli.qtree_export_levels);
    tileset.to_file(tileset_path)?;

    tileset.make_implicit(&world.grid, &quadtree);

    return Ok(());

    // Export by calling a subprocess to merge the .jsonl files and convert them to the
    // target format
    let path_output_tiles = cli.output.join("tiles");
    if !path_output_tiles.is_dir() {
        fs::create_dir_all(&path_output_tiles)?;
        info!("Created output directory {:#?}", &path_output_tiles);
    }
    let path_features_input_dir = cli.output.join("inputs");
    if !path_features_input_dir.is_dir() {
        fs::create_dir_all(&path_features_input_dir)?;
        info!("Created output directory {:#?}", &path_features_input_dir);
    }
    let cotypes_str: Vec<String> = match &world.cityobject_types {
        None => Vec::new(),
        Some(cotypes) => cotypes.iter().map(|co| co.to_string()).collect(),
    };
    let cotypes_arg = cotypes_str.join(",");

    let attribute_spec: String = match &cli.object_attribute {
        None => "".to_string(),
        Some(attributes) => attributes.join(","),
    };

    // TODO: need to refactor this parallel loop somehow that it does not only read the
    //  3d tiles tiles, but also works with cityjson output
    info!("Exporting and optimizing {} tiles", tiles.len());
    if cli.format == Formats::_3DTiles && cli.exe_gltfpack.is_none() {
        debug!("exe_gltfpack is not set, skipping gltf optimization")
    };
    tiles.into_par_iter().for_each(|tile| {
        let tileid = &tile.id;
        let qtree_nodeid: spatial_structs::QuadTreeNodeId = tileid.into();
        let qtree_node = quadtree
            .node(&qtree_nodeid)
            .unwrap_or_else(|| panic!("did not find tile {} in quadtree", tileid));
        if qtree_node.nr_items > 0 {
            let tileid = tileid.to_string();
            let file_name = tileid.clone();
            let output_file = path_output_tiles
                .join(&file_name)
                .with_extension(&subprocess_config.output_extension);
            // We write the list of feature paths for a tile into a text file, instead of passing
            // super long paths-string to the subprocess, because with very long arguments we can
            // get an 'Argument list too long' error.
            let path_features_input_file = path_features_input_dir
                .join(&file_name)
                .with_extension("input");
            fs::create_dir_all(path_features_input_file.parent().unwrap()).unwrap_or_else(|_| {
                panic!(
                    "should be able to create the directory {:?}",
                    path_features_input_file.parent().unwrap()
                )
            });
            let mut feature_input = File::create(&path_features_input_file).unwrap_or_else(|_| {
                panic!(
                    "should be able to create a file {:?}",
                    &path_features_input_file
                )
            });
            for cellid in qtree_node.cells() {
                let cell = world.grid.cell(cellid);
                for fid in cell.feature_ids.iter() {
                    let fp = world.features[*fid]
                        .path_jsonl
                        .clone()
                        .into_os_string()
                        .into_string()
                        .unwrap();
                    writeln!(feature_input, "{}", fp)
                        .expect("should be able to write feature path to the input file");
                }
            }

            // We use the quadtree node bbox here instead of the Tileset.Tile bounding
            // volume, because the Tile is in EPSG:4979 and we need the input data CRS
            let b = qtree_node.bbox(&world.grid);
            // We need to string-format all the arguments with an = separator, because that's what
            // geof can accept.
            // TODO: maybe replace the subprocess carte with std::process to remove the dependency
            let mut cmd = Exec::cmd(&subprocess_config.exe)
                .arg(&subprocess_config.script)
                .arg(format!(
                    "--output_format={}",
                    &cli.format.to_string().to_lowercase()
                ))
                .arg(format!("--output_file={}", &output_file.to_str().unwrap()))
                .arg(format!(
                    "--path_metadata={}",
                    &world.path_metadata.to_str().unwrap()
                ))
                .arg(format!(
                    "--path_features_input_file={}",
                    &path_features_input_file.to_str().unwrap()
                ))
                .arg(format!("--min_x={}", b[0]))
                .arg(format!("--min_y={}", b[1]))
                .arg(format!("--min_z={}", b[2]))
                .arg(format!("--max_x={}", b[3]))
                .arg(format!("--max_y={}", b[4]))
                .arg(format!("--max_z={}", b[5]))
                .arg(format!("--cotypes={}", &cotypes_arg))
                .arg(format!("--metadata_class={}", &metadata_class))
                .arg(format!("--attribute_spec={}", &attribute_spec))
                .arg(format!("--geometric_error={}", &tile.geometric_error));

            if cli.format == Formats::_3DTiles {
                // geof specific args
                if let Some(ref cotypes) = world.cityobject_types {
                    if cotypes.contains(&parser::CityObjectType::Building)
                        || cotypes.contains(&parser::CityObjectType::BuildingPart)
                    {
                        cmd = cmd.arg("--simplify_ratio=1.0").arg("--skip_clip=true");
                    }
                }
                if log_enabled!(Level::Debug) {
                    cmd = cmd.arg("--verbose");
                }
            }
            debug!("{}", cmd.to_cmdline_lossy());
            let res_exit_status = cmd
                .stdout(Redirection::Pipe)
                .stderr(Redirection::Merge)
                .capture();
            if let Ok(capturedata) = res_exit_status {
                let stdout = capturedata.stdout_str();
                if !capturedata.success() {
                    error!("{} conversion subprocess stdout: {}", &tileid, stdout);
                    error!(
                        "{} conversion subprocess stderr: {}",
                        &tileid,
                        capturedata.stderr_str()
                    );
                } else if !stdout.is_empty() && stdout != "\n" {
                    debug!(
                        "{} conversion subproces stdout {}",
                        &tileid,
                        capturedata.stdout_str()
                    );
                }
                if !output_file.exists() {
                    error!(
                        "{} output {:?} was not written by the subprocess",
                        &tileid, &output_file
                    );
                }
            } else if let Err(popen_error) = res_exit_status {
                error!("{}", popen_error);
            }
            // Run gltfpack on the produced glb
            if cli.format == Formats::_3DTiles {
                if let Some(ref gltfpack) = cli.exe_gltfpack {
                    let res_exit_status = Exec::cmd(gltfpack)
                        .arg("-cc")
                        .arg("-kn")
                        .arg("-i")
                        .arg(&output_file)
                        .arg("-o")
                        .arg(&output_file)
                        .stdout(Redirection::Pipe)
                        .stderr(Redirection::Merge)
                        .capture();
                    if let Ok(capturedata) = res_exit_status {
                        let stdout = capturedata.stdout_str();
                        if !capturedata.success() {
                            error!("{} gltfpack subprocess stdout: {}", &tileid, stdout);
                            error!(
                                "{} gltfpack subprocess stderr: {}",
                                &tileid,
                                capturedata.stderr_str()
                            );
                        } else if !stdout.is_empty() && stdout != "\n" {
                            debug!(
                                "{} gltfpack subproces stdout {}",
                                &tileid,
                                capturedata.stdout_str()
                            );
                        }
                    } else if let Err(popen_error) = res_exit_status {
                        error!("{}", popen_error);
                    }
                }
            }
        } else {
            debug!("tile {} is empty", &tile.id)
        }
    });
    info!("Done");
    if !log_enabled!(Level::Debug) {
        fs::remove_dir_all(path_features_input_dir)?;
    }
    Ok(())
}
