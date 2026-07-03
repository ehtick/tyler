use std::fs;
use std::path::PathBuf;

use cityjson_convert::{
    convert_to_tsv, tabulate_addresses, tabulate_cityobjects, tabulate_model_metadata,
    tabulate_semantic_assignments, tabulate_semantics, write_addresses_tsv, write_cityobjects_tsv,
    write_metadata_tsv, write_semantic_assignments_tsv, write_semantic_definitions_tsv,
    write_split_semantics_tsv, TsvExportOptions, TsvWriteOptions,
};
use cityjson_lib::cityjson_types::v2_0::OwnedAttributeValue;
use cityjson_lib::json;

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn parse_tsv(bytes: &[u8]) -> Vec<Vec<String>> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .from_reader(bytes);
    let headers = reader
        .headers()
        .expect("read TSV headers")
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let mut rows = vec![headers];
    rows.extend(reader.records().map(|record| {
        record
            .expect("read TSV row")
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    }));
    rows
}

fn temp_output_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cityjson_convert_{name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn writes_cityobjects_tsv_with_case_insensitive_collisions() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "attributes":{
                        "eindRegistratie":"first",
                        "eindregistratie":"second"
                    }
                }
            },
            "vertices":[]
        }"#,
    )
    .expect("parse collision CityJSON");
    let table = tabulate_cityobjects(&model).unwrap();

    let mut bytes = Vec::new();
    write_cityobjects_tsv(&table, &TsvWriteOptions::default(), &mut bytes).unwrap();
    let rows = parse_tsv(&bytes);

    assert_eq!(
        rows[0],
        [
            "cityobject_id",
            "cityobject_type",
            "attributes__eindRegistratie",
            "attributes__eindregistratie__2"
        ]
    );
    assert_eq!(rows[1][2], "first");
    assert_eq!(rows[1][3], "second");
}

#[test]
fn omits_geometry_ref_attributes_from_cityobjects_tsv() {
    let mut model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"1",
                        "boundaries":[[[0,1,2,0]]]
                    }]
                }
            },
            "vertices":[[0,0,0],[1,0,0],[0,1,0]]
        }"#,
    )
    .expect("parse inline CityJSON");
    let geometry_handle = model
        .cityobjects()
        .iter()
        .next()
        .and_then(|(_, object)| object.geometry())
        .and_then(|geometries| geometries.first().copied())
        .expect("geometry handle");
    let (_, cityobject) = model
        .cityobjects_mut()
        .iter_mut()
        .next()
        .expect("cityobject");
    cityobject.attributes_mut().insert(
        "location".to_string(),
        OwnedAttributeValue::Geometry(geometry_handle),
    );

    let table = tabulate_cityobjects(&model).expect("tabulate CityObjects");
    let mut bytes = Vec::new();
    write_cityobjects_tsv(&table, &TsvWriteOptions::default(), &mut bytes).unwrap();
    let rows = parse_tsv(&bytes);

    assert!(!rows[0].contains(&"attributes__location".to_string()));
}

#[test]
fn writes_split_address_tsv_with_multipoint_wkb_and_dynamic_columns() {
    let mut model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiPoint",
                        "lod":"1",
                        "boundaries":[0]
                    }]
                }
            },
            "vertices":[[4,5,6]]
        }"#,
    )
    .expect("parse inline CityJSON");
    let geometry_handle = model
        .cityobjects()
        .iter()
        .next()
        .and_then(|(_, object)| object.geometry())
        .and_then(|geometries| geometries.first().copied())
        .expect("geometry handle");
    let (_, cityobject) = model
        .cityobjects_mut()
        .iter_mut()
        .next()
        .expect("cityobject");
    cityobject.extra_mut().insert(
        "address".to_string(),
        OwnedAttributeValue::Map(std::collections::HashMap::from([
            (
                "location".to_string(),
                OwnedAttributeValue::Geometry(geometry_handle),
            ),
            (
                "street".to_string(),
                OwnedAttributeValue::String("Main Street".to_string()),
            ),
            (
                "houseNumber".to_string(),
                OwnedAttributeValue::String("7".to_string()),
            ),
        ])),
    );

    let expected_wkb =
        cityjson_convert::tabular::geometry_ref_to_multipoint_wkb(&model, geometry_handle)
            .expect("encode address location as WKB");
    let table = tabulate_addresses(&model).expect("tabulate addresses");
    let mut bytes = Vec::new();
    write_addresses_tsv(
        &table,
        &TsvWriteOptions {
            include_null_rows: false,
            include_hierarchy: false,
            include_cityjson_ordinal: true,
        },
        &mut bytes,
    )
    .unwrap();
    let rows = parse_tsv(&bytes);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], "cityobject_id");
    assert_eq!(rows[0][2], "cityobject_ix");
    assert!(rows[0].contains(&"location_wkb".to_string()));
    assert!(rows[0].contains(&"street".to_string()));
    assert!(rows[0].contains(&"houseNumber".to_string()));
    let location_ix = rows[0]
        .iter()
        .position(|name| name == "location_wkb")
        .expect("location_wkb column");
    assert_eq!(rows[1][location_ix], bytes_to_hex(&expected_wkb));
}

#[test]
fn writes_cityobjects_tsv_with_filtering_ordinal_hierarchy_and_json_cells() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "children":["room"],
                    "attributes":{"name":"Library","scores":[1,2]}
                },
                "room":{
                    "type":"BuildingRoom",
                    "parents":["building"],
                    "attributes":{"name":null}
                }
            },
            "vertices":[]
        }"#,
    )
    .unwrap();
    let table = tabulate_cityobjects(&model).unwrap();

    let mut bytes = Vec::new();
    write_cityobjects_tsv(
        &table,
        &TsvWriteOptions {
            include_null_rows: false,
            include_hierarchy: true,
            include_cityjson_ordinal: true,
        },
        &mut bytes,
    )
    .unwrap();
    let rows = parse_tsv(&bytes);

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        [
            "cityobject_id",
            "cityobject_type",
            "cityobject_ix",
            "parents",
            "children",
            "attributes__name",
            "attributes__scores",
        ]
    );
    assert_eq!(rows[1][0], "building");
    assert_eq!(rows[1][2], "0");
    assert_eq!(rows[1][3], "[]");
    assert_eq!(rows[1][4], "[\"room\"]");
    assert_eq!(rows[1][5], "Library");
    assert_eq!(rows[1][6], "[1,2]");

    let mut bytes = Vec::new();
    write_cityobjects_tsv(
        &table,
        &TsvWriteOptions {
            include_null_rows: true,
            include_hierarchy: false,
            include_cityjson_ordinal: false,
        },
        &mut bytes,
    )
    .unwrap();
    assert_eq!(parse_tsv(&bytes).len(), 3);
}

#[test]
fn writes_metadata_tsv_with_fixed_fields_extent_and_extra() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{},
            "vertices":[],
            "metadata":{
                "identifier":"dataset-1",
                "referenceDate":"2026-01-02",
                "referenceSystem":"EPSG:7415",
                "title":"Demo",
                "geographicalExtent":[1.0,2.0,3.0,4.0,5.0,6.0],
                "+quality":{"score":7}
            }
        }"#,
    )
    .unwrap();
    let table = tabulate_model_metadata(&model).unwrap();

    let mut bytes = Vec::new();
    write_metadata_tsv(&table, &mut bytes).unwrap();
    let rows = parse_tsv(&bytes);

    assert_eq!(rows.len(), 2);
    assert!(rows[0].contains(&"geographical_extent".to_string()));
    assert!(rows[0].contains(&"geographical_extent_wkt".to_string()));
    assert!(rows[0].contains(&"metadata_extra__+quality__score".to_string()));
    assert_eq!(rows[1][0], "dataset-1");
    assert_eq!(rows[1][4], "[1.0,2.0,3.0,4.0,5.0,6.0]");
    assert_eq!(rows[1][5], "POLYGON((1 2, 4 2, 4 5, 1 5, 1 2))");
    assert_eq!(rows[1].last().unwrap(), "7");
}

#[test]
fn writes_semantic_definition_and_assignment_tsvs() {
    let model = json::from_slice(semantic_fixture()).unwrap();
    let semantics = tabulate_semantics(&model).unwrap();
    let assignments = tabulate_semantic_assignments(&model).unwrap();

    let mut definitions = Vec::new();
    write_semantic_definitions_tsv(
        &semantics,
        &TsvWriteOptions {
            include_null_rows: true,
            include_hierarchy: true,
            include_cityjson_ordinal: false,
        },
        &mut definitions,
    )
    .unwrap();
    let rows = parse_tsv(&definitions);
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0],
        [
            "semantic_id",
            "semantic_type",
            "parent",
            "children",
            "attributes__slope"
        ]
    );
    assert_eq!(rows[1][1], "RoofSurface");
    assert_eq!(rows[1][3], "[1]");
    assert_eq!(rows[1][4], "30");
    assert_eq!(rows[2][2], "0");

    let mut assignment_bytes = Vec::new();
    write_semantic_assignments_tsv(
        &assignments,
        &TsvWriteOptions {
            include_null_rows: true,
            include_hierarchy: false,
            include_cityjson_ordinal: true,
        },
        &mut assignment_bytes,
    )
    .unwrap();
    let rows = parse_tsv(&assignment_bytes);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows[0],
        [
            "semantic_id",
            "cityobject_id",
            "cityobject_ix",
            "geometry_ix",
            "geometry_type",
            "geometry_lod",
            "primitive_ix",
        ]
    );
    assert_eq!(rows[1][0], "0");
    assert_eq!(rows[2][0], "1");
    assert_eq!(rows[3][0], "");
}

#[test]
fn writes_split_semantics_as_joined_filtered_tsv() {
    let model = json::from_slice(semantic_fixture()).unwrap();
    let semantics = tabulate_semantics(&model).unwrap();
    let assignments = tabulate_semantic_assignments(&model).unwrap();

    let mut bytes = Vec::new();
    write_split_semantics_tsv(
        &semantics,
        &assignments,
        &TsvWriteOptions {
            include_null_rows: false,
            include_hierarchy: true,
            include_cityjson_ordinal: true,
        },
        &mut bytes,
    )
    .unwrap();
    let rows = parse_tsv(&bytes);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], "semantic_id");
    assert_eq!(rows[0][2], "cityobject_ix");
    assert!(rows[0].contains(&"semantic_type".to_string()));
    assert!(rows[0].contains(&"parent".to_string()));
    assert!(rows[0].contains(&"children".to_string()));
    assert!(rows[0].contains(&"attributes__slope".to_string()));
    assert_eq!(rows[1][0], "0");
    assert_eq!(rows[1][1], "building");
    assert!(rows[1].contains(&"RoofSurface".to_string()));
    assert_eq!(rows[1].last().unwrap(), "30");

    let mut bytes = Vec::new();
    write_split_semantics_tsv(
        &semantics,
        &assignments,
        &TsvWriteOptions {
            include_null_rows: true,
            include_hierarchy: false,
            include_cityjson_ordinal: false,
        },
        &mut bytes,
    )
    .unwrap();
    assert_eq!(parse_tsv(&bytes).len(), 4);
}

#[test]
fn converts_model_to_tsv_directory_outputs() {
    let model = json::from_slice(semantic_fixture()).unwrap();
    let dir = temp_output_dir("tsv_convert");

    convert_to_tsv(
        &model,
        &dir,
        &TsvExportOptions {
            include_null_rows: true,
            include_hierarchy: true,
            include_cityjson_ordinal: true,
            include_metadata: true,
            split_semantics: true,
            split_address: true,
        },
    )
    .unwrap();

    assert!(dir.join("cityobjects.tsv").is_file());
    assert!(dir.join("metadata.tsv").is_file());
    assert!(dir.join("semantics.tsv").is_file());
    assert!(dir.join("addresses.tsv").is_file());

    fs::remove_dir_all(dir).unwrap();
}

fn semantic_fixture() -> &'static [u8] {
    br#"{
        "type":"CityJSON",
        "version":"2.0",
        "CityObjects":{
            "building":{
                "type":"Building",
                "attributes":{"name":"Library"},
                "geometry":[{
                    "type":"MultiSurface",
                    "lod":"2.0",
                    "boundaries":[[[0,1,2]],[[0,2,3]],[[1,2,3]]],
                    "semantics":{
                        "surfaces":[
                            {"type":"RoofSurface","children":[1],"slope":30},
                            {"type":"WallSurface","parent":0}
                        ],
                        "values":[0,1,null]
                    }
                }]
            }
        },
        "vertices":[[0,0,0],[1,0,0],[0,1,0],[0,0,1]],
        "metadata":{"identifier":"semantic-fixture"}
    }"#
}
