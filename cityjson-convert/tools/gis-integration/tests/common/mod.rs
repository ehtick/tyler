use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use cityjson_convert::{convert_to_gpkg, GpkgExportOptions};
use cityjson_lib::{json, CityModel};
use rusqlite::Connection;

#[allow(dead_code)]
pub struct Case {
    pub name: &'static str,
    pub table_name: &'static str,
    pub expected_gpkg_type: &'static str,
    pub expected_postgis_type: &'static str,
    pub assert_planar_valid: bool,
    source: &'static [u8],
}

impl Case {
    pub fn model(&self) -> Result<CityModel> {
        json::from_slice(self.source).with_context(|| format!("parse {} CityJSON", self.name))
    }

    pub fn write_gpkg(&self, dir: &Path) -> Result<PathBuf> {
        let path = dir.join(format!("{}.gpkg", self.name));
        convert_to_gpkg(&self.model()?, &path, &GpkgExportOptions::default())
            .with_context(|| format!("write {} GeoPackage", self.name))?;
        Ok(path)
    }
}

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "multipoint_z",
            table_name: "building_multipoint",
            expected_gpkg_type: "MULTIPOINT",
            expected_postgis_type: "ST_MultiPoint",
            assert_planar_valid: true,
            source: br#"{
                "type":"CityJSON",
                "version":"2.0",
                "CityObjects":{
                    "building":{"type":"Building","geometry":[{"type":"MultiPoint","lod":"1","boundaries":[0,1,2]}]}
                },
                "vertices":[[0,0,0],[10,0,1],[10,10,2]],
                "metadata":{"referenceSystem":"EPSG:7415"}
            }"#,
        },
        Case {
            name: "multilinestring_z",
            table_name: "building_multilinestring",
            expected_gpkg_type: "MULTILINESTRING",
            expected_postgis_type: "ST_MultiLineString",
            assert_planar_valid: true,
            source: br#"{
                "type":"CityJSON",
                "version":"2.0",
                "CityObjects":{
                    "building":{"type":"Building","geometry":[{"type":"MultiLineString","lod":"1","boundaries":[[0,1,2],[3,4,5]]}]}
                },
                "vertices":[[0,0,0],[10,0,1],[10,10,2],[20,0,0],[30,0,1],[30,10,2]],
                "metadata":{"referenceSystem":"EPSG:7415"}
            }"#,
        },
        Case {
            name: "multisurface_with_hole_z",
            table_name: "building_multisurface",
            expected_gpkg_type: "MULTIPOLYGON",
            expected_postgis_type: "ST_MultiPolygon",
            assert_planar_valid: true,
            source: br#"{
                "type":"CityJSON",
                "version":"2.0",
                "CityObjects":{
                    "building":{"type":"Building","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,3,0],[4,5,6,7,4]]]}]}
                },
                "vertices":[[0,0,0],[10,0,1],[10,10,2],[0,10,3],[2,2,4],[4,2,5],[4,4,6],[2,4,7]],
                "metadata":{"referenceSystem":"EPSG:7415"}
            }"#,
        },
        Case {
            name: "solid_flattened_z",
            table_name: "building_solid",
            expected_gpkg_type: "MULTIPOLYGON",
            expected_postgis_type: "ST_MultiPolygon",
            assert_planar_valid: false,
            source: br#"{
                "type":"CityJSON",
                "version":"2.0",
                "CityObjects":{
                    "building":{"type":"Building","geometry":[{"type":"Solid","lod":"2","boundaries":[[[[0,1,2,3,0]],[[4,7,6,5,4]]]]}]}
                },
                "vertices":[[0,0,0],[10,0,0],[10,10,0],[0,10,0],[0,0,10],[10,0,10],[10,10,10],[0,10,10]],
                "metadata":{"referenceSystem":"EPSG:7415"}
            }"#,
        },
    ]
}

#[allow(dead_code)]
pub fn geometry_blob(path: &Path, table_name: &str) -> Result<Vec<u8>> {
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.query_row(
        &format!("SELECT geom FROM \"{table_name}\" LIMIT 1"),
        [],
        |row| row.get(0),
    )
    .with_context(|| format!("read {table_name}.geom"))
}

#[allow(dead_code)]
pub fn geometry_type_name(path: &Path, table_name: &str) -> Result<String> {
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.query_row(
        "SELECT geometry_type_name FROM gpkg_geometry_columns WHERE table_name = ?1",
        [table_name],
        |row| row.get(0),
    )
    .with_context(|| format!("read {table_name} geometry metadata"))
}

#[allow(dead_code)]
pub fn gpkg_payload_wkb(blob: &[u8]) -> Result<&[u8]> {
    if blob.len() < 8 || &blob[0..2] != b"GP" {
        bail!("invalid GeoPackageBinary header");
    }
    let envelope_code = (blob[3] >> 1) & 0b111;
    let envelope_bytes = match envelope_code {
        0 => 0,
        1 => 32,
        2 | 3 => 48,
        4 => 64,
        _ => bail!("unsupported GeoPackageBinary envelope code {envelope_code}"),
    };
    let payload_offset = 8 + envelope_bytes;
    if blob.len() <= payload_offset {
        bail!("GeoPackageBinary blob has no WKB payload");
    }
    Ok(&blob[payload_offset..])
}
