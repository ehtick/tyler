//! GDAL acceptance tests for complete GeoPackage files produced by Tyler.

mod common;

use std::process::Command;

/// Purpose: validate GeoPackage files through GDAL's public reader.
/// Input: complete temporary GeoPackages for the supported multi-geometry families.
/// Assertions: GDAL loads each file, exposes its XYZ layer and feature, and reports EPSG:7415.
#[test]
fn ogrinfo_opens_complete_generated_geopackages() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    for case in common::cases() {
        let path = case.write_gpkg(dir.path())?;
        let output = Command::new("ogrinfo")
            .args(["-ro", "-al"])
            .arg(&path)
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "ogrinfo failed for {}\nstdout:\n{}\nstderr:\n{}",
            case.name,
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&format!("Layer name: {}", case.table_name)),
            "{} missing layer:\n{}",
            case.name,
            stdout
        );
        assert!(
            stdout.contains(case.expected_ogr_geometry),
            "{} missing XYZ geometry type:\n{}",
            case.name,
            stdout
        );
        assert!(
            stdout.contains("Feature Count: 1"),
            "{} missing feature count:\n{}",
            case.name,
            stdout
        );
        assert!(
            stdout.contains(case.decoded_geometry),
            "{} missing decoded feature geometry:\n{}",
            case.name,
            stdout
        );
        assert!(
            stdout.contains("7415"),
            "{} missing EPSG:7415:\n{}",
            case.name,
            stdout
        );
    }
    Ok(())
}
