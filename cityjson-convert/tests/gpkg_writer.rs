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

fn table_column_type(conn: &Connection, table: &str, column: &str) -> String {
    conn.query_row(
        &format!("SELECT type FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |row| row.get(0),
    )
    .expect("read column type")
}

fn read_gpb_envelope(blob: &[u8]) -> [f64; 6] {
    let mut values = [0.0; 6];
    for (ix, chunk) in blob[8..56].chunks_exact(8).enumerate() {
        values[ix] = f64::from_le_bytes(chunk.try_into().expect("envelope value bytes"));
    }
    values
}

fn read_blob_prefix(conn: &Connection, table: &str) -> Vec<u8> {
    conn.query_row(
        &format!("SELECT geom FROM \"{table}\" LIMIT 1"),
        [],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .expect("read geometry blob")
}

fn layers_relations_metadata_model() -> cityjson_lib::CityModel {
    json::from_slice(
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
            "vertices":[[0,0,0],[1,0,0],[0,1,0],[1,1,0]],
            "metadata":{
                "identifier":"dataset-1",
                "referenceSystem":"https://www.opengis.net/def/crs/EPSG/0/7415"
            }
        }"#,
    )
    .expect("parse inline CityJSON")
}

fn assert_layers_relations_metadata_tables(conn: &Connection) {
    let tables = read_table_names(conn);
    for expected in [
        "gpkg_contents",
        "gpkg_geometry_columns",
        "gpkg_spatial_ref_sys",
        "cityobject_hierarchy",
        "semantic_hierarchy",
        "building_multisurface",
        "buildingroom_multisurface",
    ] {
        assert!(
            tables.iter().any(|table| table == expected),
            "missing table {expected}"
        );
    }

    assert_eq!(table_row_count(conn, "cityobject_hierarchy"), 1);
    assert_eq!(table_row_count(conn, "semantic_hierarchy"), 0);
    assert_eq!(table_row_count(conn, "building_multisurface"), 1);
}

fn metadata_output_path(output: &std::path::Path) -> PathBuf {
    let stem = output
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("metadata");
    output.with_file_name(format!("{stem}_metadata.gpkg"))
}

fn assert_metadata_gpkg(output: &std::path::Path) {
    let metadata_output = metadata_output_path(output);
    assert!(metadata_output.is_file());
    let conn = Connection::open(&metadata_output).expect("open metadata GeoPackage");
    assert!(table_exists(&conn, "metadata"));
    assert_eq!(table_row_count(&conn, "metadata"), 1);
    assert_eq!(
        table_column_type(&conn, "metadata", "geographical_extent_wkb"),
        "BLOB"
    );
    let identifier: String = conn
        .query_row("SELECT identifier FROM metadata", [], |row| row.get(0))
        .expect("read metadata identifier");
    assert_eq!(identifier, "dataset-1");
}

fn assert_building_layer_metadata(conn: &Connection) {
    let (geometry_type_name, z, m): (String, i64, i64) = conn
        .query_row(
            "SELECT geometry_type_name, z, m FROM gpkg_geometry_columns WHERE table_name = 'building_multisurface'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read geometry column metadata");
    assert_eq!(geometry_type_name, "MULTIPOLYGON");
    assert_eq!((z, m), (1, 0));
    assert_eq!(
        table_column_type(conn, "building_multisurface", "geom"),
        "MULTIPOLYGON"
    );

    let (min_x, min_y, max_x, max_y): (f64, f64, f64, f64) = conn
        .query_row(
            "SELECT min_x, min_y, max_x, max_y FROM gpkg_contents WHERE table_name = 'building_multisurface'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read layer extent");
    assert_eq!((min_x, min_y, max_x, max_y), (0.0, 0.0, 1.0, 1.0));
}

fn assert_building_blob_header(conn: &Connection) {
    let blob = read_blob_prefix(conn, "building_multisurface");
    assert_eq!(&blob[0..2], b"GP");
    assert_eq!(blob[2], 0);
    assert_eq!(blob[3] & 0b0000_0001, 0b0000_0001);
    assert_eq!(blob[3] & 0b0000_0100, 0b0000_0100);
    assert_eq!(
        i32::from_le_bytes(blob[4..8].try_into().expect("srs id bytes")),
        7415
    );
    let envelope = read_gpb_envelope(&blob);
    for (actual, expected) in envelope.into_iter().zip([0.0, 1.0, 0.0, 1.0, 0.0, 0.0]) {
        assert!((actual - expected).abs() < f64::EPSILON);
    }
    assert_eq!(
        u32::from_le_bytes(blob[57..61].try_into().expect("wkb type bytes")),
        1006
    );
}

fn assert_crs_wkt_metadata(conn: &Connection) {
    let (definition, definition_12_063): (String, String) = conn
        .query_row(
            "SELECT definition, definition_12_063 FROM gpkg_spatial_ref_sys WHERE srs_id = 7415",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read CRS definition");
    assert_ne!(definition, "undefined");
    assert_eq!(definition, definition_12_063);
    assert!(definition.contains("EPSG"));

    let extension_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM gpkg_extensions WHERE extension_name = 'gpkg_crs_wkt' AND table_name = 'gpkg_spatial_ref_sys' AND column_name = 'definition_12_063'",
            [],
            |row| row.get(0),
        )
        .expect("read CRS WKT extension row");
    assert_eq!(extension_count, 1);
}

fn assert_attribute_table_registered(conn: &Connection, table_name: &str) {
    let data_type: String = conn
        .query_row(
            "SELECT data_type FROM gpkg_contents WHERE table_name = ?1",
            [table_name],
            |row| row.get(0),
        )
        .expect("read gpkg_contents row");
    assert_eq!(data_type, "attributes");
}

fn assert_single_cityobject_hierarchy_edge(conn: &Connection) {
    let relation: (String, String) = conn
        .query_row(
            "SELECT parent_id, child_id FROM cityobject_hierarchy",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read relation row");
    assert_eq!(relation, ("building".to_string(), "room".to_string()));
}

#[test]
fn converts_model_to_gpkg_with_feature_layers_relations_and_metadata() {
    let model = layers_relations_metadata_model();
    let output = stable_output_path("convert_to_gpkg_layers_relations_metadata");
    let metadata_output = metadata_output_path(&output);
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }
    if metadata_output.exists() {
        fs::remove_file(&metadata_output).expect("remove previous metadata output");
    }

    convert_to_gpkg(
        &model,
        &output,
        &GpkgExportOptions {
            split_lod: false,
            include_semantics: false,
            include_address: false,
            include_hierarchy: true,
            include_metadata: true,
            output_crs: None,
        },
    )
    .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    assert_layers_relations_metadata_tables(&conn);
    assert_building_layer_metadata(&conn);
    assert_building_blob_header(&conn);
    assert_crs_wkt_metadata(&conn);
    assert_single_cityobject_hierarchy_edge(&conn);
    assert_attribute_table_registered(&conn, "cityobject_hierarchy");
    assert_metadata_gpkg(&output);

    let metadata_output = metadata_output_path(&output);
    if output.exists() {
        fs::remove_file(&output).expect("clean up output");
    }
    if metadata_output.exists() {
        fs::remove_file(&metadata_output).expect("clean up metadata output");
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
            include_semantics: true,
            include_address: false,
            include_hierarchy: true,
            include_metadata: false,
            output_crs: None,
        },
    )
    .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    assert!(table_exists(&conn, "building_multisurface_lod2_0"));
    assert!(table_exists(&conn, "semantics"));
    assert!(!table_exists(&conn, "semantic_relations"));
    assert!(table_exists(&conn, "semantic_hierarchy"));
    assert_eq!(table_row_count(&conn, "semantics"), 2);
    assert_eq!(table_row_count(&conn, "semantic_hierarchy"), 1);
    assert_eq!(table_column_type(&conn, "semantics", "geom"), "GEOMETRY");
    let semantics_data_type: String = conn
        .query_row(
            "SELECT data_type FROM gpkg_contents WHERE table_name = 'semantics'",
            [],
            |row| row.get(0),
        )
        .expect("read semantics gpkg_contents row");
    assert_eq!(semantics_data_type, "features");

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
fn reuses_feature_layer_for_multiple_cityobjects_with_same_type_and_lod() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building-1":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"1",
                        "boundaries":[[[0,1,2,0]]]
                    }]
                },
                "building-2":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"1",
                        "boundaries":[[[3,4,5,3]]]
                    }]
                }
            },
            "vertices":[[0,0,0],[1,0,0],[0,1,0],[2,0,0],[3,0,0],[2,1,0]],
            "metadata":{"referenceSystem":"EPSG:7415"}
        }"#,
    )
    .expect("parse inline CityJSON");
    let output = stable_output_path("convert_to_gpkg_reuses_feature_layer");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    convert_to_gpkg(
        &model,
        &output,
        &GpkgExportOptions {
            split_lod: true,
            include_semantics: false,
            include_address: false,
            include_hierarchy: false,
            include_metadata: false,
            output_crs: None,
        },
    )
    .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    assert!(table_exists(&conn, "building_multisurface_lod1"));
    assert!(!table_exists(&conn, "building_multisurface_lod1_2"));
    assert_eq!(table_row_count(&conn, "building_multisurface_lod1"), 2);

    if output.exists() {
        fs::remove_file(&output).expect("clean up output");
    }
}

#[test]
fn converts_model_to_gpkg_with_case_insensitive_attribute_collisions() {
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
                    },
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
    .expect("parse collision CityJSON");
    let output = stable_output_path("convert_to_gpkg_case_insensitive_collisions");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    convert_to_gpkg(&model, &output, &GpkgExportOptions::default())
        .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    let column_names = conn
        .prepare("SELECT name FROM pragma_table_info('building_multisurface') ORDER BY cid")
        .expect("prepare column query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query column names")
        .map(|row| row.expect("read column name"))
        .collect::<Vec<_>>();
    assert!(column_names.contains(&"attributes__eindRegistratie".to_string()));
    assert!(column_names.contains(&"attributes__eindregistratie__2".to_string()));

    let (first, second): (String, String) = conn
        .query_row(
            r#"SELECT "attributes__eindRegistratie", "attributes__eindregistratie__2" FROM building_multisurface LIMIT 1"#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read collision row");
    assert_eq!(first, "first");
    assert_eq!(second, "second");

    if output.exists() {
        fs::remove_file(&output).expect("clean up output");
    }
}

#[test]
fn omits_geometry_ref_attributes_from_feature_layers() {
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

    let output = stable_output_path("convert_to_gpkg_omits_geometry_ref_attribute");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    convert_to_gpkg(&model, &output, &GpkgExportOptions::default())
        .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    let has_column: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('building_multisurface') WHERE name = 'attributes__location'",
            [],
            |row| row.get(0),
        )
        .expect("read feature columns");
    assert_eq!(has_column, 0);

    if output.exists() {
        fs::remove_file(&output).expect("clean up output");
    }
}

#[test]
fn converts_include_address_to_multipoint_feature_layer() {
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
            "vertices":[[4,5,6]],
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
    cityobject.extra_mut().insert(
        "address".to_string(),
        OwnedAttributeValue::Vec(vec![OwnedAttributeValue::Map(
            std::collections::HashMap::from([
                (
                    "location".to_string(),
                    OwnedAttributeValue::Geometry(geometry_handle),
                ),
                (
                    "street".to_string(),
                    OwnedAttributeValue::String("Main Street".to_string()),
                ),
            ]),
        )]),
    );

    let output = stable_output_path("convert_to_gpkg_include_address");
    if output.exists() {
        fs::remove_file(&output).expect("remove previous output");
    }

    convert_to_gpkg(
        &model,
        &output,
        &GpkgExportOptions {
            split_lod: false,
            include_semantics: false,
            include_address: true,
            include_hierarchy: false,
            include_metadata: false,
            output_crs: None,
        },
    )
    .expect("GeoPackage conversion should succeed");

    let conn = Connection::open(&output).expect("open GeoPackage");
    assert!(table_exists(&conn, "addresses"));
    assert_eq!(table_row_count(&conn, "addresses"), 1);
    let geometry_type_name: String = conn
        .query_row(
            "SELECT geometry_type_name FROM gpkg_geometry_columns WHERE table_name = 'addresses'",
            [],
            |row| row.get(0),
        )
        .expect("read address geometry column metadata");
    assert_eq!(geometry_type_name, "MULTIPOINT");
    assert_eq!(table_column_type(&conn, "addresses", "geom"), "MULTIPOINT");

    let (street, blob): (String, Vec<u8>) = conn
        .query_row("SELECT street, geom FROM addresses LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("read address row");
    assert_eq!(street, "Main Street");
    assert_eq!(&blob[0..2], b"GP");

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
