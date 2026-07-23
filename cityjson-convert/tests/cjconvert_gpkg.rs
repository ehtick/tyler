//! CLI acceptance tests for `cjconvert` `GeoPackage` options.

use rusqlite::Connection;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn corpus_file(relative: &str) -> PathBuf {
    let root = env::var_os("CITYJSON_CORPUS_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cityjson-corpus"),
        PathBuf::from,
    );
    let path = root.join(relative);
    assert!(path.is_file(), "CityJSON corpus fixture is missing: {}. Set CITYJSON_CORPUS_DIR to a cityjson-corpus checkout.", path.display());
    path
}
fn fake_complete() -> PathBuf {
    corpus_file("cases/conformance/v2_0/cityjson_fake_complete/cityjson_fake_complete.city.json")
}
fn address_fixture() -> PathBuf {
    corpus_file(
        "cases/conformance/v2_0/cityobject_building_address/cityobject_building_address.city.json",
    )
}
fn convert(input: &Path, option: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temporary output directory");
    let output = dir.path().join("output.gpkg");
    let mut command = Command::new(env!("CARGO_BIN_EXE_cjconvert"));
    command.args(["--format", "gpkg", "--output"]);
    command.arg(&output);
    if let Some(option) = option {
        command.arg(option);
    }
    let result = command.arg(input).output().expect("run cjconvert");
    assert!(
        result.status.success(),
        "cjconvert failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    (dir, output)
}
fn has_table(output: &Path, table: &str) -> bool {
    Connection::open(output)
        .expect("open GeoPackage")
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = \x27table\x27 AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .is_ok()
}

/// Purpose: verify default CLI output omits every optional `GeoPackage` product.
/// Input: the canonical complete `CityJSON` fixture.
/// Assertions: optional tables and metadata sidecar are absent.
#[test]
fn defaults_omit_optional_outputs() {
    let (_dir, output) = convert(&fake_complete(), None);
    assert!(!has_table(&output, "semantics"));
    assert!(!has_table(&output, "cityobject_hierarchy"));
    assert!(!output.with_file_name("output_metadata.gpkg").exists());
}
/// Purpose: verify the `LoD` split option controls layer naming.
/// Input: the canonical complete `CityJSON` fixture with only --gpkg-split-lod added.
/// Assertions: a `GeoPackage` layer includes a `LoD` fragment.
#[test]
fn split_lod_creates_lod_layers() {
    let (_dir, output) = convert(&fake_complete(), Some("--gpkg-split-lod"));
    let conn = Connection::open(output).unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT table_name FROM gpkg_geometry_columns")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        names.iter().any(|name| name.contains("_lod")),
        "expected a LoD-split layer: {names:?}"
    );
}
/// Purpose: verify semantic export is opt-in.
/// Input: the canonical complete `CityJSON` fixture with only --gpkg-include-semantics added.
/// Assertions: the semantics feature table exists.
#[test]
fn include_semantics_writes_semantics_table() {
    let (_dir, output) = convert(&fake_complete(), Some("--gpkg-include-semantics"));
    assert!(has_table(&output, "semantics"));
}
/// Purpose: verify hierarchy export is opt-in.
/// Input: the canonical complete `CityJSON` fixture with only --gpkg-include-hierarchy added.
/// Assertions: the `CityObject` hierarchy table exists.
#[test]
fn include_hierarchy_writes_hierarchy_table() {
    let (_dir, output) = convert(&fake_complete(), Some("--gpkg-include-hierarchy"));
    assert!(has_table(&output, "cityobject_hierarchy"));
}
/// Purpose: verify address export is opt-in.
/// Input: the canonical address `CityJSON` fixture with only --gpkg-include-address added.
/// Assertions: the address feature layer exists.
#[test]
fn include_address_writes_address_layer() {
    let (_dir, output) = convert(&address_fixture(), Some("--gpkg-include-address"));
    assert!(has_table(&output, "addresses"));
}
/// Purpose: verify source metadata export is opt-in.
/// Input: the canonical complete `CityJSON` fixture with only --gpkg-include-metadata added.
/// Assertions: the metadata sidecar `GeoPackage` exists.
#[test]
fn include_metadata_writes_metadata_sidecar() {
    let (_dir, output) = convert(&fake_complete(), Some("--gpkg-include-metadata"));
    assert!(output.with_file_name("output_metadata.gpkg").is_file());
}
