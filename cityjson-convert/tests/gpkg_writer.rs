//! Focused `GeoPackage` writer edge-case coverage.

use std::fs;
use std::path::Path;

use cityjson_convert::{
    aggregate_metadata_gpkg, convert_to_gpkg, write_metadata_gpkg, GpkgExportOptions,
    GpkgMetadataFragment,
};
use cityjson_lib::{json, CityModel};
use rusqlite::Connection;
use tempfile::TempDir;

fn model(objects: &str, reference_system: Option<&str>) -> CityModel {
    let metadata = reference_system.map_or_else(String::new, |value| {
        format!(r#","metadata":{{"referenceSystem":"{value}"}}"#)
    });
    let document = format!(
        r#"{{"type":"CityJSON","version":"2.0","CityObjects":{{{objects}}},"vertices":[[0,0,0],[1,0,0],[0,1,0],[2,0,0],[3,0,0],[2,1,0]]{metadata}}}"#
    );
    json::from_slice(document.as_bytes()).expect("parse inline CityJSON")
}

fn write(model: &CityModel, directory: &TempDir) -> Connection {
    let output = directory.path().join("output.gpkg");
    convert_to_gpkg(model, &output, &GpkgExportOptions::default()).expect("write GeoPackage");
    Connection::open(output).expect("open GeoPackage")
}

fn column_names(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .expect("prepare column query")
        .query_map([table], |row| row.get(0))
        .expect("query columns")
        .map(|row| row.expect("read column name"))
        .collect()
}

/// Purpose: retain one layer for `CityObjects` with an identical type, family, and `LoD`.
/// Input: two `Building` `MultiSurfaces` at `LoD` 1.
/// Assertions: the shared layer contains both geometry rows.
#[test]
fn reuses_layer_for_matching_type_family_and_lod() {
    let model = model(
        r#""building-1":{"type":"Building","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,0]]]}]},"building-2":{"type":"Building","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[3,4,5,3]]]}]}"#,
        Some("EPSG:7415"),
    );
    let directory = TempDir::new().expect("create temporary directory");
    let conn = write(&model, &directory);
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM building_multisurface", [], |row| {
            row.get(0)
        })
        .expect("count rows");
    assert_eq!(rows, 2);
}

/// Purpose: prevent projected attributes from one `CityObject` type leaking into another schema.
/// Input: a Building with `name` and a `BuildingPart` without attributes.
/// Assertions: only the Building layer has the projected `name` column.
#[test]
fn isolates_schema_by_cityobject_type() {
    let model = model(
        r#""building":{"type":"Building","attributes":{"name":"Library"},"geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,0]]]}]},"part":{"type":"BuildingPart","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,2,3,0]]]}]}"#,
        Some("EPSG:7415"),
    );
    let directory = TempDir::new().expect("create temporary directory");
    let conn = write(&model, &directory);
    assert!(column_names(&conn, "building_multisurface").contains(&"attributes__name".to_string()));
    assert!(
        !column_names(&conn, "buildingpart_multisurface").contains(&"attributes__name".to_string())
    );
}

/// Purpose: preserve absent attributes as SQL NULL in a shared layer.
/// Input: two `BuildingParts`, only one with `roofType`.
/// Assertions: the shared column exists and exactly one row is NULL.
#[test]
fn writes_absent_shared_layer_attributes_as_null() {
    let model = model(
        r#""part-1":{"type":"BuildingPart","attributes":{"roofType":"flat"},"geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,0]]]}]},"part-2":{"type":"BuildingPart","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,2,3,0]]]}]}"#,
        Some("EPSG:7415"),
    );
    let directory = TempDir::new().expect("create temporary directory");
    let conn = write(&model, &directory);
    let nulls: i64 = conn.query_row(r#"SELECT COUNT(*) FROM buildingpart_multisurface WHERE "attributes__roofType" IS NULL"#, [], |row| row.get(0)).expect("count NULL values");
    assert_eq!(nulls, 1);
}

/// Purpose: disambiguate attribute names that collide under `SQLite` case-insensitive matching.
/// Input: one Building with `eindRegistratie` and `eindregistratie`.
/// Assertions: both values use distinct, deterministic columns.
#[test]
fn disambiguates_case_insensitive_attribute_columns() {
    let model = model(
        r#""building":{"type":"Building","attributes":{"eindRegistratie":"first","eindregistratie":"second"},"geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,0]]]}]}"#,
        Some("EPSG:7415"),
    );
    let directory = TempDir::new().expect("create temporary directory");
    let conn = write(&model, &directory);
    let values: (String, String) = conn.query_row(r#"SELECT "attributes__eindRegistratie", "attributes__eindregistratie__2" FROM building_multisurface"#, [], |row| Ok((row.get(0)?, row.get(1)?))).expect("read collision values");
    assert_eq!(values, ("first".to_string(), "second".to_string()));
}

/// Purpose: reject source metadata that cannot declare an EPSG `GeoPackage` CRS before output changes.
/// Input: otherwise-valid models with missing and non-EPSG reference systems.
/// Assertions: conversion fails, the error identifies the metadata requirement, and a sentinel output is untouched.
#[test]
fn rejects_missing_or_non_epsg_source_crs_before_writing() {
    for reference_system in [None, Some("OGC:CRS84")] {
        let model = model(
            r#""building":{"type":"Building","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,0]]]}]}"#,
            reference_system,
        );
        let directory = TempDir::new().expect("create temporary directory");
        let output = directory.path().join("sentinel.gpkg");
        fs::write(&output, b"keep me").expect("write sentinel");
        let error = convert_to_gpkg(&model, &output, &GpkgExportOptions::default())
            .expect_err("reject non-EPSG CRS");
        assert!(error
            .to_string()
            .contains("referenceSystem must contain a parseable EPSG"));
        assert_eq!(
            fs::read(Path::new(&output)).expect("read sentinel"),
            b"keep me"
        );
    }
}

/// Purpose: preserve deterministic tile identity and GeoPackage paths while aggregating metadata.
/// Input: two compatible per-tile metadata GeoPackages.
/// Assertions: rows follow fragment order and the spatial metadata registration remains valid.
#[test]
fn aggregates_tile_metadata_geopackages() {
    let model = model(
        r#""building":{"type":"Building","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,0]]]}]}"#,
        Some("EPSG:7415"),
    );
    let directory = TempDir::new().expect("create temporary directory");
    let first = directory.path().join("first.gpkg");
    let second = directory.path().join("second.gpkg");
    write_metadata_gpkg(&model, &first).expect("write first metadata fragment");
    write_metadata_gpkg(&model, &second).expect("write second metadata fragment");

    let output = directory.path().join("metadata.gpkg");
    aggregate_metadata_gpkg(
        &output,
        &[
            GpkgMetadataFragment {
                tile_id: "0/0/0".to_string(),
                gpkg_path: "t/0/0/0.gpkg".to_string(),
                metadata_path: first,
            },
            GpkgMetadataFragment {
                tile_id: "1/2/3".to_string(),
                gpkg_path: "t/1/2/3.gpkg".to_string(),
                metadata_path: second,
            },
        ],
    )
    .expect("aggregate metadata fragments");

    let conn = Connection::open(output).expect("open aggregate metadata");
    let rows = conn
        .prepare("SELECT tile_id, gpkg_path FROM metadata ORDER BY id")
        .expect("prepare aggregate row query")
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query aggregate rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read aggregate rows");
    assert_eq!(
        rows,
        vec![
            ("0/0/0".to_string(), "t/0/0/0.gpkg".to_string()),
            ("1/2/3".to_string(), "t/1/2/3.gpkg".to_string()),
        ]
    );
    let registration: (String, String, i32) = conn
        .query_row(
            "SELECT table_name, column_name, srs_id FROM gpkg_geometry_columns WHERE table_name = 'metadata'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read geometry registration");
    assert_eq!(
        registration,
        (
            "metadata".to_string(),
            "geographical_extent_wkb".to_string(),
            7415
        )
    );
}
