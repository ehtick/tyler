use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use cityjson_convert::{convert_to_glb, ExportOptions};
use cityjson_lib::json;
use serde_json::Value;

fn test_output_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "cityjson-convert-{name}-{}-{unique}.glb",
        std::process::id()
    ))
}

fn read_glb_json(bytes: &[u8]) -> Value {
    assert!(bytes.len() >= 20, "glb file should contain a header and JSON chunk");
    assert_eq!(&bytes[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);

    let declared_length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    assert_eq!(declared_length, bytes.len());

    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(&bytes[16..20], b"JSON");

    serde_json::from_slice(&bytes[20..20 + json_length]).expect("GLB JSON chunk should parse")
}

#[test]
fn convert_to_glb_writes_expected_geometry_layout() {
    let model = json::merge_feature_stream_slice(include_bytes!("data/ams_up_building_part.city.jsonl"))
        .expect("fixture feature stream should parse");
    let output_path = test_output_path("ams-up-geometry");

    convert_to_glb(&model, &output_path, &ExportOptions::default())
        .expect("GLB conversion should succeed");

    let glb_bytes = fs::read(&output_path).expect("test GLB should be written");
    let root = read_glb_json(&glb_bytes);

    let accessors = root["accessors"]
        .as_array()
        .expect("glTF should contain accessors");
    assert_eq!(accessors.len(), 3, "writer currently emits position, normal, and index accessors");

    let positions = &accessors[0];
    let normals = &accessors[1];
    let indices = &accessors[2];

    let position_count = positions["count"].as_u64().unwrap();
    let normal_count = normals["count"].as_u64().unwrap();
    let index_count = indices["count"].as_u64().unwrap();

    assert!(position_count > 0, "mesh should contain vertices");
    assert_eq!(position_count, normal_count);
    assert!(index_count > position_count, "triangle soup should be deduplicated into indexed geometry");
    assert_eq!(index_count % 3, 0, "index stream should describe triangles");
    assert_eq!(indices["componentType"].as_u64().unwrap(), 5123, "small meshes should use u16 indices");

    let min = positions["min"].as_array().expect("positions accessor should have min");
    let max = positions["max"].as_array().expect("positions accessor should have max");
    for axis in 0..3 {
        assert!(min[axis].as_f64().unwrap() < 0.0);
        assert!(max[axis].as_f64().unwrap() > 0.0);
    }

    let node_matrix = root["nodes"][0]["matrix"]
        .as_array()
        .expect("root node should carry the local-to-world transform");
    assert_eq!(node_matrix.len(), 16);
    assert_eq!(node_matrix[0].as_f64().unwrap(), 1.0);
    assert_eq!(node_matrix[5].as_f64().unwrap(), 0.0);
    assert_eq!(node_matrix[6].as_f64().unwrap(), -1.0);
    assert_eq!(node_matrix[9].as_f64().unwrap(), 1.0);
    assert_eq!(node_matrix[10].as_f64().unwrap(), 0.0);
    assert_ne!(node_matrix[12].as_f64().unwrap(), 0.0);
    assert_ne!(node_matrix[13].as_f64().unwrap(), 0.0);
    assert_ne!(node_matrix[14].as_f64().unwrap(), 0.0);

    let mesh = &root["meshes"][0]["primitives"][0];
    if let Some(mode) = mesh.get("mode") {
        assert_eq!(mode.as_u64().unwrap(), 4);
    }
    assert_eq!(mesh["attributes"]["POSITION"].as_u64().unwrap(), 0);
    assert_eq!(mesh["attributes"]["NORMAL"].as_u64().unwrap(), 1);
    assert_eq!(mesh["indices"].as_u64().unwrap(), 2);

    let _ = fs::remove_file(output_path);
}

#[test]
#[ignore = "TODO: add a fixture with an interior ring and assert triangulation succeeds without dropping the hole"]
fn convert_to_glb_triangulates_surfaces_with_holes() {
    todo!("add a CityJSONFeature fixture with a hole and assert the emitted mesh/index layout");
}

#[test]
#[ignore = "TODO: add a multi-feature fixture and assert merged bounds/root centering stay stable"]
fn convert_to_glb_merges_multiple_features_into_one_centered_mesh() {
    todo!("add a merged feature-stream fixture and validate bounds, transform, and index selection");
}

#[test]
#[ignore = "TODO: enable once quantization lands in the geometry pipeline"]
fn convert_to_glb_quantizes_geometry_when_enabled() {
    todo!("assert accessor component types, dequantization transform, and acceptable coordinate error");
}

#[test]
#[ignore = "TODO: enable once EXT_meshopt_compression is written by the exporter"]
fn convert_to_glb_writes_meshopt_compression_extension() {
    todo!("assert EXT_meshopt_compression declarations and compressed bufferView metadata");
}
