use std::fs;
use std::path::PathBuf;

use cityjson_convert::{convert_to_glb, ExportOptions};
use cityjson_lib::json;
use serde_json::Value;

fn stable_output_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("output")
        .join(format!("{name}.glb"))
}

fn read_glb_json(bytes: &[u8]) -> Value {
    assert!(
        bytes.len() >= 20,
        "glb file should contain a header and JSON chunk"
    );
    assert_eq!(&bytes[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);

    let declared_length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    assert_eq!(declared_length, bytes.len());

    let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    assert_eq!(&bytes[16..20], b"JSON");

    serde_json::from_slice(&bytes[20..20 + json_length]).expect("GLB JSON chunk should parse")
}

fn has_extension(root: &Value, name: &str) -> bool {
    root["extensionsUsed"]
        .as_array()
        .is_some_and(|extensions| extensions.iter().any(|value| value.as_str() == Some(name)))
}

fn accessor_component_types(root: &Value) -> Vec<u64> {
    root["accessors"]
        .as_array()
        .expect("glTF should contain accessors")
        .iter()
        .map(|accessor| accessor["componentType"].as_u64().unwrap())
        .collect()
}

#[test]
fn convert_to_glb_writes_expected_geometry_layout() {
    let model = json::merge_feature_stream_slice(include_bytes!("data/ams_up_holes.city.jsonl"))
        .expect("fixture feature stream should parse");
    let output_path = stable_output_path("ams-up-geometry");

    convert_to_glb(&model, &output_path, &ExportOptions::default())
        .expect("GLB conversion should succeed");

    let glb_bytes = fs::read(&output_path).expect("test GLB should be written");
    let root = read_glb_json(&glb_bytes);

    let accessors = root["accessors"]
        .as_array()
        .expect("glTF should contain accessors");
    assert_eq!(accessors.len(), 4);

    let positions = &accessors[0];
    let normals = &accessors[1];
    let feature_ids = &accessors[2];
    let indices = &accessors[3];

    let position_count = positions["count"].as_u64().unwrap();
    let normal_count = normals["count"].as_u64().unwrap();
    let index_count = indices["count"].as_u64().unwrap();

    assert!(position_count > 0, "mesh should contain vertices");
    assert_eq!(position_count, normal_count);
    assert_eq!(position_count, feature_ids["count"].as_u64().unwrap());
    assert!(
        index_count > position_count,
        "triangle soup should be deduplicated into indexed geometry"
    );
    assert_eq!(index_count % 3, 0, "index stream should describe triangles");
    assert_eq!(
        positions["componentType"].as_u64().unwrap(),
        5122,
        "positions should be quantized to i16"
    );
    assert_eq!(
        normals["componentType"].as_u64().unwrap(),
        5120,
        "normals should be quantized to i8"
    );
    assert_eq!(positions["normalized"].as_bool().unwrap(), true);
    assert_eq!(normals["normalized"].as_bool().unwrap(), true);
    assert!(matches!(
        feature_ids["componentType"].as_u64().unwrap(),
        5121 | 5123
    ));
    assert!(matches!(indices["componentType"].as_u64().unwrap(), 5123 | 5125));

    let extensions_used = root["extensionsUsed"]
        .as_array()
        .expect("quantized glTF should declare extensionsUsed");
    let extensions_required = root["extensionsRequired"]
        .as_array()
        .expect("quantized glTF should declare extensionsRequired");
    assert!(extensions_used
        .iter()
        .any(|value| value.as_str() == Some("KHR_mesh_quantization")));
    assert!(extensions_used
        .iter()
        .any(|value| value.as_str() == Some("EXT_meshopt_compression")));
    assert!(extensions_used
        .iter()
        .any(|value| value.as_str() == Some("EXT_mesh_features")));
    assert!(extensions_used
        .iter()
        .any(|value| value.as_str() == Some("EXT_structural_metadata")));
    assert!(extensions_required
        .iter()
        .any(|value| value.as_str() == Some("KHR_mesh_quantization")));
    assert!(extensions_required
        .iter()
        .any(|value| value.as_str() == Some("EXT_meshopt_compression")));

    let buffers = root["buffers"]
        .as_array()
        .expect("compressed glTF should contain explicit source and fallback buffers");
    assert_eq!(buffers.len(), 2);

    let min = positions["min"]
        .as_array()
        .expect("positions accessor should have min");
    let max = positions["max"]
        .as_array()
        .expect("positions accessor should have max");
    for axis in 0..3 {
        assert!(min[axis].as_f64().unwrap() < 0.0);
        assert!(max[axis].as_f64().unwrap() > 0.0);
    }

    let node_matrix = root["nodes"][0]["matrix"]
        .as_array()
        .expect("root node should carry the local-to-world transform");
    assert_eq!(node_matrix.len(), 16);
    let dequant_scale = node_matrix[0].as_f64().unwrap();
    assert!(dequant_scale > 0.0);
    assert_eq!(node_matrix[5].as_f64().unwrap(), 0.0);
    assert_eq!(node_matrix[6].as_f64().unwrap(), -dequant_scale);
    assert_eq!(node_matrix[9].as_f64().unwrap(), dequant_scale);
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
    assert_eq!(mesh["attributes"]["_FEATURE_ID_0"].as_u64().unwrap(), 2);
    assert_eq!(mesh["indices"].as_u64().unwrap(), 3);
    assert_eq!(
        mesh["extensions"]["EXT_mesh_features"]["featureIds"][0]["propertyTable"]
            .as_u64()
            .unwrap(),
        0
    );
    assert!(root["extensions"]["EXT_structural_metadata"]["propertyTables"]
        .as_array()
        .is_some_and(|tables| !tables.is_empty()));
}

#[test]
fn convert_to_glb_can_disable_quantization_and_compression() {
    let model = json::merge_feature_stream_slice(include_bytes!("data/ams_up_holes.city.jsonl"))
        .expect("fixture feature stream should parse");
    let output_path = stable_output_path("ams-up-raw");
    let options = ExportOptions {
        quantize_geometry: false,
        meshopt_compression: false,
        ..ExportOptions::default()
    };

    convert_to_glb(&model, &output_path, &options).expect("raw GLB conversion should succeed");

    let glb_bytes = fs::read(&output_path).expect("test GLB should be written");
    let root = read_glb_json(&glb_bytes);

    let component_types = accessor_component_types(&root);
    assert_eq!(component_types[0], 5126);
    assert_eq!(component_types[1], 5126);
    assert!(matches!(component_types[2], 5121 | 5123));
    assert!(matches!(component_types[3], 5123 | 5125));
    assert!(!has_extension(&root, "KHR_mesh_quantization"));
    assert!(!has_extension(&root, "EXT_meshopt_compression"));
    assert!(has_extension(&root, "EXT_mesh_features"));
    assert!(has_extension(&root, "EXT_structural_metadata"));

    let buffers = root["buffers"]
        .as_array()
        .expect("glTF should contain buffers");
    assert_eq!(
        buffers.len(),
        1,
        "uncompressed output should contain a single GLB buffer"
    );

    let buffer_views = root["bufferViews"]
        .as_array()
        .expect("glTF should contain bufferViews");
    for (index, buffer_view) in buffer_views.iter().enumerate() {
        assert_eq!(
            buffer_view["buffer"].as_u64().unwrap(),
            0,
            "bufferView {index} should point at the GLB BIN buffer"
        );
        assert!(
            buffer_view.get("extensions").is_none(),
            "bufferView {index} should not carry meshopt extension metadata"
        );
    }

    let node_matrix = root["nodes"][0]["matrix"]
        .as_array()
        .expect("root node should carry the local-to-world transform");
    assert_eq!(node_matrix[0].as_f64().unwrap(), 1.0);
    assert_eq!(node_matrix[6].as_f64().unwrap(), -1.0);
    assert_eq!(node_matrix[9].as_f64().unwrap(), 1.0);
}

#[test]
fn convert_to_glb_can_disable_only_meshopt_compression() {
    let model = json::merge_feature_stream_slice(include_bytes!("data/ams_up_holes.city.jsonl"))
        .expect("fixture feature stream should parse");
    let output_path = stable_output_path("ams-up-quantized-uncompressed");
    let options = ExportOptions {
        meshopt_compression: false,
        ..ExportOptions::default()
    };

    convert_to_glb(&model, &output_path, &options)
        .expect("quantized uncompressed GLB conversion should succeed");

    let glb_bytes = fs::read(&output_path).expect("test GLB should be written");
    let root = read_glb_json(&glb_bytes);
    let accessors = root["accessors"]
        .as_array()
        .expect("glTF should contain accessors");

    assert_eq!(accessors[0]["componentType"].as_u64().unwrap(), 5122);
    assert_eq!(accessors[1]["componentType"].as_u64().unwrap(), 5120);
    assert!(matches!(
        accessors[2]["componentType"].as_u64().unwrap(),
        5121 | 5123
    ));
    assert!(has_extension(&root, "KHR_mesh_quantization"));
    assert!(!has_extension(&root, "EXT_meshopt_compression"));
    assert!(has_extension(&root, "EXT_mesh_features"));
    assert!(has_extension(&root, "EXT_structural_metadata"));
    assert_eq!(
        root["buffers"]
            .as_array()
            .expect("glTF should contain buffers")
            .len(),
        1,
        "disabling meshopt compression should remove the fallback buffer"
    );

    let buffer_views = root["bufferViews"]
        .as_array()
        .expect("glTF should contain bufferViews");
    for (index, buffer_view) in buffer_views.iter().enumerate() {
        assert_eq!(
            buffer_view["buffer"].as_u64().unwrap(),
            0,
            "bufferView {index} should point at the GLB BIN buffer"
        );
        assert!(
            buffer_view.get("extensions").is_none(),
            "bufferView {index} should not carry meshopt extension metadata"
        );
    }
}

#[test]
fn convert_to_glb_merges_multiple_features_into_one_centered_mesh() {
    let model = json::merge_feature_stream_slice(include_bytes!("data/multi_feature_types.city.jsonl"))
        .expect("fixture feature stream should parse");
    let output_path = stable_output_path("multi-feature-types");

    convert_to_glb(&model, &output_path, &ExportOptions::default())
        .expect("GLB conversion should succeed");

    let glb_bytes = fs::read(&output_path).expect("test GLB should be written");
    let root = read_glb_json(&glb_bytes);

    let primitives = root["meshes"][0]["primitives"]
        .as_array()
        .expect("glTF should contain primitives");
    assert_eq!(primitives.len(), 2, "writer should group geometry into one primitive per feature type");

    let materials = root["materials"]
        .as_array()
        .expect("glTF should contain materials");
    assert_eq!(materials.len(), 2, "writer should create one material per primitive type group");
    assert_ne!(
        materials[0]["pbrMetallicRoughness"]["baseColorFactor"],
        materials[1]["pbrMetallicRoughness"]["baseColorFactor"],
        "different feature types should receive different default colors"
    );

    for primitive in primitives {
        assert!(primitive["attributes"].get("_FEATURE_ID_0").is_some());
        assert_eq!(
            primitive["extensions"]["EXT_mesh_features"]["featureIds"][0]["featureCount"]
                .as_u64()
                .unwrap(),
            1
        );
    }

    assert_eq!(
        root["extensions"]["EXT_structural_metadata"]["propertyTables"][0]["count"]
            .as_u64()
            .unwrap(),
        2
    );
}

#[test]
fn convert_to_glb_writes_meshopt_compression_extension() {
    let model = json::merge_feature_stream_slice(include_bytes!("data/ams_up_holes.city.jsonl"))
        .expect("fixture feature stream should parse");
    let output_path = stable_output_path("ams-up-meshopt");

    convert_to_glb(&model, &output_path, &ExportOptions::default())
        .expect("GLB conversion should succeed");

    let glb_bytes = fs::read(&output_path).expect("test GLB should be written");
    let root = read_glb_json(&glb_bytes);

    let buffers = root["buffers"]
        .as_array()
        .expect("compressed glTF should declare buffers");
    assert_eq!(
        buffers.len(),
        2,
        "compressed output should carry a source buffer and a fallback placeholder"
    );
    assert!(
        buffers[1]["extensions"]["EXT_meshopt_compression"]["fallback"]
            .as_bool()
            .unwrap(),
        "fallback buffer should be explicitly tagged"
    );

    let buffer_views = root["bufferViews"]
        .as_array()
        .expect("compressed glTF should declare bufferViews");
    let meshopt_views = buffer_views
        .iter()
        .filter(|buffer_view| buffer_view.get("extensions").is_some())
        .collect::<Vec<_>>();
    assert_eq!(meshopt_views.len(), 4);

    for (index, buffer_view) in meshopt_views.iter().enumerate() {
        assert_eq!(
            buffer_view["buffer"].as_u64().unwrap(),
            1,
            "bufferView {index} should reference the fallback buffer layout"
        );
        let extension = &buffer_view["extensions"]["EXT_meshopt_compression"];
        assert_eq!(
            extension["buffer"].as_u64().unwrap(),
            0,
            "bufferView {index} should source compressed bytes from buffer 0"
        );
        assert!(extension["byteLength"].as_u64().unwrap() > 0);
        assert!(extension["count"].as_u64().unwrap() > 0);
    }

    assert_eq!(
        meshopt_views[0]["extensions"]["EXT_meshopt_compression"]["mode"]
            .as_str()
            .unwrap(),
        "ATTRIBUTES"
    );
    assert_eq!(
        meshopt_views[1]["extensions"]["EXT_meshopt_compression"]["mode"]
            .as_str()
            .unwrap(),
        "ATTRIBUTES"
    );
    assert_eq!(
        meshopt_views[2]["extensions"]["EXT_meshopt_compression"]["mode"]
            .as_str()
            .unwrap(),
        "ATTRIBUTES"
    );
    assert_eq!(
        meshopt_views[3]["extensions"]["EXT_meshopt_compression"]["mode"]
            .as_str()
            .unwrap(),
        "TRIANGLES"
    );
}
