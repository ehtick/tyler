mod common;

use std::process::Command;

#[test]
fn ogrinfo_opens_generated_geopackages() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    for case in common::cases() {
        let path = case.write_gpkg(dir.path())?;
        assert_eq!(
            common::geometry_type_name(&path, case.table_name)?,
            case.expected_gpkg_type,
            "{}",
            case.name
        );

        let output = Command::new("ogrinfo")
            .arg("-ro")
            .arg("-so")
            .arg(&path)
            .arg(case.table_name)
            .output()?;
        assert!(
            output.status.success(),
            "ogrinfo failed for {}\nstdout:\n{}\nstderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(case.table_name),
            "{} missing layer",
            case.name
        );
        assert!(
            stdout.contains(expected_gdal_geometry_name(case.expected_gpkg_type)),
            "{} missing geometry type in ogrinfo output:\n{}",
            case.name,
            stdout
        );
        assert!(
            stdout.contains("EPSG"),
            "{} missing CRS metadata",
            case.name
        );
    }
    Ok(())
}

fn expected_gdal_geometry_name(gpkg_type: &str) -> &str {
    match gpkg_type {
        "MULTIPOINT" => "3D Multi Point",
        "MULTILINESTRING" => "3D Multi Line String",
        "MULTIPOLYGON" => "3D Multi Polygon",
        value => value,
    }
}
