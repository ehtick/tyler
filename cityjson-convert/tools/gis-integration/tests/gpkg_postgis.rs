mod common;

use postgres::{Client, NoTls};

#[test]
fn postgis_loads_generated_geopackage_payload_wkb() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let mut client = Client::connect(&cityjson_convert_gis_integration::database_url(), NoTls)?;
    client.batch_execute("CREATE EXTENSION IF NOT EXISTS postgis")?;

    for case in common::cases() {
        let path = case.write_gpkg(dir.path())?;
        let blob = common::geometry_blob(&path, case.table_name)?;
        let wkb = common::gpkg_payload_wkb(&blob)?;
        let row = client.query_one(
            "
            WITH geom AS (
                SELECT ST_SetSRID(ST_GeomFromWKB($1), 7415) AS g
            )
            SELECT
                ST_GeometryType(g) AS geometry_type,
                ST_NDims(g)::integer AS ndims,
                ST_SRID(g)::integer AS srid,
                ST_IsValid(g) AS valid
            FROM geom
            ",
            &[&wkb],
        )?;

        let geometry_type: String = row.get("geometry_type");
        let ndims: i32 = row.get("ndims");
        let srid: i32 = row.get("srid");
        let valid: bool = row.get("valid");

        assert_eq!(geometry_type, case.expected_postgis_type, "{}", case.name);
        assert_eq!(ndims, 3, "{}", case.name);
        assert_eq!(srid, 7415, "{}", case.name);
        if case.assert_planar_valid {
            assert!(valid, "{} should be PostGIS-valid", case.name);
        }
    }
    Ok(())
}
