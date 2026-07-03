use std::fs;
use std::path::PathBuf;

use cityjson_convert::{convert_to_gpkg, GpkgExportOptions};
use cityjson_lib::cityjson_types::v2_0::OwnedAttributeValue;
use cityjson_lib::json;
use rusqlite::Connection;

fn stable_output_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("output")
        .join(format!("{name}.gpkg"))
}

fn read_table_names(conn: &Connection) -> Vec<String> {
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare sqlite_master query");
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query sqlite_master")
        .map(|row| row.expect("read table name"))
        .collect()
}

fn table_row_count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
        row.get(0)
    })
    .expect("count table rows")
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .is_ok()
}

fn read_blob_prefix(conn: &Connection, table: &str) -> Vec<u8> {
    conn.query_row(
        &format!("SELECT geom FROM \"{table}\" LIMIT 1"),
        [],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .expect("read geometry blob")
}

#[test]
fn converts_model_to_gpkg_with_feature_layers_relations_and_metadata() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "children":["room"],
                    "attributes":{"name":"Library"},
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"1",
                        "boundaries":[[[0,1,2,0]]]
                    }]
                },
                "room":{
                    "type":"BuildingRoom",
                    "parents":["building"],
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"1",
                        "boundaries":[[[0,2,3,0]]]
                    }]
                }
            },
            "vertices":[
                [0,0,0],
                [1,0,0],
                [0,1,0],
                [1,1,0]
            ],
            "metadata":{
                "identifier":"dataset-1",
                "referenceSystem":"https://www.opengis.net/def/crs/EPSG/0/7415"
            }
        }"#,
    )
    .expect("parse inline CityJSON");
    let output = stable_output_path("convert_to_gpkg_layers_relations_metadata");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    convert_to_gpkg(
        &model,
        &output,
        &GpkgExportOptions {
            split_lod: false,
            split_semantics: false,
            include_metadata: true,
            output_crs: None,
        },
    )
    .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    let tables = read_table_names(&conn);
    for expected in [
        "gpkg_contents",
        "gpkg_geometry_columns",
        "gpkg_metadata",
        "gpkg_metadata_reference",
        "gpkg_spatial_ref_sys",
        "cityobject_relations",
        "building_multisurface",
        "buildingroom_multisurface",
    ] {
        assert!(
            tables.iter().any(|table| table == expected),
            "missing table {expected}"
        );
    }

    assert_eq!(table_row_count(&conn, "cityobject_relations"), 1);
    assert_eq!(table_row_count(&conn, "gpkg_metadata"), 1);
    assert_eq!(table_row_count(&conn, "gpkg_metadata_reference"), 1);
    assert_eq!(table_row_count(&conn, "building_multisurface"), 1);

    let (geometry_type_name, z, m): (String, i64, i64) = conn
        .query_row(
            "SELECT geometry_type_name, z, m FROM gpkg_geometry_columns WHERE table_name = 'building_multisurface'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read geometry column metadata");
    assert_eq!(geometry_type_name, "MULTIPOLYGON");
    assert_eq!((z, m), (1, 0));

    let (min_x, min_y, max_x, max_y): (f64, f64, f64, f64) = conn
        .query_row(
            "SELECT min_x, min_y, max_x, max_y FROM gpkg_contents WHERE table_name = 'building_multisurface'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read layer extent");
    assert_eq!((min_x, min_y, max_x, max_y), (0.0, 0.0, 1.0, 1.0));

    let blob = read_blob_prefix(&conn, "building_multisurface");
    assert_eq!(&blob[0..2], b"GP");
    assert_eq!(blob[2], 0);
    assert_eq!(blob[3] & 0b0000_0001, 0b0000_0001);
    assert_eq!(blob[3] & 0b0000_0100, 0b0000_0100);
    assert_eq!(
        i32::from_le_bytes(blob[4..8].try_into().expect("srs id bytes")),
        7415
    );
    assert_eq!(
        u32::from_le_bytes(blob[57..61].try_into().expect("wkb type bytes")),
        1006
    );

    let relation: (String, String) = conn
        .query_row(
            "SELECT parent_cityobject_id, child_cityobject_id FROM cityobject_relations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read relation row");
    assert_eq!(relation, ("building".to_string(), "room".to_string()));

    if output.exists() {
        fs::remove_file(&output).expect("clean up output");
    }
}

#[test]
fn converts_model_to_gpkg_with_split_lod_and_semantics() {
    let model = json::from_slice(
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
                        "boundaries":[[[0,1,2,0]],[[0,2,3,0]],[[1,2,3,1]]],
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
            "vertices":[
                [0,0,0],
                [1,0,0],
                [0,1,0],
                [0,0,1]
            ],
            "metadata":{
                "identifier":"semantic-fixture",
                "referenceSystem":"EPSG:7415"
            }
        }"#,
    )
    .expect("parse inline CityJSON");
    let output = stable_output_path("convert_to_gpkg_split_lod_semantics");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    convert_to_gpkg(
        &model,
        &output,
        &GpkgExportOptions {
            split_lod: true,
            split_semantics: true,
            include_metadata: false,
            output_crs: None,
        },
    )
    .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    assert!(table_exists(&conn, "building_multisurface_lod2_0"));
    assert!(table_exists(&conn, "semantics"));
    assert!(table_exists(&conn, "semantic_relations"));
    assert_eq!(table_row_count(&conn, "semantics"), 2);
    assert_eq!(table_row_count(&conn, "semantic_relations"), 3);

    let inserted_lod: String = conn
        .query_row(
            "SELECT lod FROM \"building_multisurface_lod2_0\" LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read feature lod");
    assert_eq!(inserted_lod, "2.0");

    if output.exists() {
        fs::remove_file(&output).expect("clean up output");
    }
}

#[test]
fn converts_geometry_ref_attributes_to_raw_wkb_blobs() {
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
            "vertices":[[0,0,0],[1,0,0],[0,1,0]],
            "metadata":{"referenceSystem":"EPSG:7415"}
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
    let expected_wkb = cityjson_convert::tabular::geometry_ref_to_wkb(&model, geometry_handle)
        .expect("encode geometry attribute as WKB");

    let output = stable_output_path("convert_to_gpkg_geometry_ref_attribute");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    convert_to_gpkg(&model, &output, &GpkgExportOptions::default())
        .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    let declared_type: String = conn
        .query_row(
            "SELECT type FROM pragma_table_info('building_multisurface') WHERE name = 'attributes__location'",
            [],
            |row| row.get(0),
        )
        .expect("read geometry attribute column type");
    assert_eq!(declared_type, "BLOB");

    let (attribute_blob, feature_blob): (Vec<u8>, Vec<u8>) = conn
        .query_row(
            "SELECT attributes__location, geom FROM building_multisurface LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read geometry attribute and feature blobs");
    assert_eq!(attribute_blob, expected_wkb);
    assert_eq!(&feature_blob[0..2], b"GP");
    assert_ne!(feature_blob, attribute_blob);

    if output.exists() {
        fs::remove_file(&output).expect("clean up output");
    }
}

#[test]
fn convert_to_gpkg_requires_a_parseable_crs() {
    let model = json::from_slice(
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
            "vertices":[
                [0,0,0],
                [1,0,0],
                [0,1,0]
            ]
        }"#,
    )
    .expect("parse inline CityJSON");
    let output = stable_output_path("convert_to_gpkg_missing_crs");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    let error = convert_to_gpkg(&model, &output, &GpkgExportOptions::default())
        .expect_err("conversion should fail without CRS metadata");
    assert!(error.to_string().contains("gpkg-output-crs"));
    assert!(
        !output.exists(),
        "writer should fail before creating the file"
    );
}
