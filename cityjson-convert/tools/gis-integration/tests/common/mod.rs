//! Shared CityJSON fixtures for GDAL GeoPackage interoperability tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cityjson_convert::{convert_to_gpkg, GpkgExportOptions};
use cityjson_lib::{json, CityModel};

pub struct Case {
    pub name: &'static str,
    pub table_name: &'static str,
    pub expected_ogr_geometry: &'static str,
    pub decoded_geometry: &'static str,
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
            expected_ogr_geometry: "3D Multi Point",
            decoded_geometry: "MULTIPOINT Z",
            source: br#"{
                "type":"CityJSON", "version":"2.0",
                "CityObjects":{"building":{"type":"Building","geometry":[{"type":"MultiPoint","lod":"1","boundaries":[0,1,2]}]}},
                "vertices":[[0,0,0],[10,0,1],[10,10,2]],
                "metadata":{"referenceSystem":"EPSG:7415"}
            }"#,
        },
        Case {
            name: "multilinestring_z",
            table_name: "building_multilinestring",
            expected_ogr_geometry: "3D Multi Line String",
            decoded_geometry: "MULTILINESTRING Z",
            source: br#"{
                "type":"CityJSON", "version":"2.0",
                "CityObjects":{"building":{"type":"Building","geometry":[{"type":"MultiLineString","lod":"1","boundaries":[[0,1,2],[3,4,5]]}]}},
                "vertices":[[0,0,0],[10,0,1],[10,10,2],[20,0,0],[30,0,1],[30,10,2]],
                "metadata":{"referenceSystem":"EPSG:7415"}
            }"#,
        },
        Case {
            name: "multisurface_z",
            table_name: "building_multisurface",
            expected_ogr_geometry: "3D Multi Polygon",
            decoded_geometry: "MULTIPOLYGON Z",
            source: br#"{
                "type":"CityJSON", "version":"2.0",
                "CityObjects":{"building":{"type":"Building","geometry":[{"type":"MultiSurface","lod":"1","boundaries":[[[0,1,2,3,0],[4,5,6,7,4]]]}]}},
                "vertices":[[0,0,0],[10,0,1],[10,10,2],[0,10,3],[2,2,4],[4,2,5],[4,4,6],[2,4,7]],
                "metadata":{"referenceSystem":"EPSG:7415"}
            }"#,
        },
    ]
}
