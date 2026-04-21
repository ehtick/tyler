use std::path::PathBuf;

use cityjson_lib::json;
use clap::Parser;

use cityjson_convert::{convert_to_glb, ExportOptions};

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Input `CityJSON` file (.city.json).
    input: PathBuf,
    /// Path to the output GLB file.
    #[arg(short, long)]
    output: PathBuf,
    /// Default PBR base color for the generated GLB.
    #[arg(long = "native-glb-color", default_value = "#FFC0CB")]
    native_glb_color: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let cli = Cli::parse();

    let model = json::from_file(&cli.input)?;
    let options = ExportOptions {
        native_glb_color: cli.native_glb_color,
    };

    convert_to_glb(&model, cli.output, &options)?;
    Ok(())
}
