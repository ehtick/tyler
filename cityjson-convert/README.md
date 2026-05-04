# cityjson-convert

`cityjson-convert` converts CityJSON data to other formats.

At the moment it supports glTF output, both through the library API and through the `cjconvert` CLI.
More output formats may be added in future releases.

## Library

Use the library when you want to convert a parsed `CityModel` to a GLB file:

```rust
use cityjson_convert::{convert_to_glb, ExportOptions, GeometryPlacement};
use cityjson_lib::CityModel;

fn export(model: &CityModel) -> anyhow::Result<()> {
    let options = ExportOptions {
        native_glb_color: "#FFC0CB".to_string(),
        metadata_class_name: "cityobject".to_string(),
        feature_type_colors: Default::default(),
        geometry_placement: GeometryPlacement::SourceCoordinates,
        clip_bbox: None,
        clip_geographic_region: None,
        smooth_normals: false,
        quantize_geometry: true,
        meshopt_compression: true,
    };

    convert_to_glb(model, "output/tiles.glb", &options)
}
```

## CLI

Convert a CityJSON file to a GLB file from the command line:

```shell
cjconvert input.city.json --output output/model.glb
```

You can also customize the output:

```shell
cjconvert input.city.json \
  --output output/model.glb \
  --native-glb-color "#FFC0CB" \
  --3dtiles-metadata-class cityobject \
  --smooth-normals
```
