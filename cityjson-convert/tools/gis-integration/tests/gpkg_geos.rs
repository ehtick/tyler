mod common;

use geos::{Geom, Geometry};

#[test]
fn geos_loads_generated_geopackage_payload_wkb() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    for case in common::cases() {
        let path = case.write_gpkg(dir.path())?;
        let blob = common::geometry_blob(&path, case.table_name)?;
        let wkb = common::gpkg_payload_wkb(&blob)?;
        let geometry = Geometry::new_from_wkb(wkb)?;
        if case.assert_planar_valid {
            assert!(geometry.is_valid()?, "{} should be GEOS-valid", case.name);
        }
    }
    Ok(())
}
