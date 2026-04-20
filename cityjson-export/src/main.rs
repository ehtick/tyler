use std::path::PathBuf;

use clap::Parser;

use cityjson_export::{export_glb, ExportOptions};

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Input dataset root. This can be a legacy CityJSONFeatures directory or a cjindex dataset root.
    input: PathBuf,
    /// Path to the output GLB file.
    #[arg(short, long)]
    output: PathBuf,
    /// The CityObject type to include in the output. Can be specified multiple times.
    #[arg(long, value_enum)]
    object_type: Option<Vec<cityjson_export::parser::CityObjectType>>,
    /// Grid cell size for the input indexing step.
    #[arg(long, default_value = "250")]
    grid_cellsize: u32,
    /// Clamp the minimum z coordinate used to build the grid.
    #[arg(long)]
    grid_minz: Option<i32>,
    /// Clamp the maximum z coordinate used to build the grid.
    #[arg(long)]
    grid_maxz: Option<i32>,
    /// Default PBR base color for the generated GLB.
    #[arg(long = "native-glb-color", default_value = "#FFC0CB")]
    native_glb_color: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let cli = Cli::parse();

    let options = ExportOptions {
        object_types: cli.object_type,
        grid_cellsize: cli.grid_cellsize,
        grid_minz: cli.grid_minz,
        grid_maxz: cli.grid_maxz,
        native_glb_color: cli.native_glb_color,
    };

    export_glb(cli.input, cli.output, &options)?;
    Ok(())
}
