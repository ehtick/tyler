use std::collections::BTreeMap;

use cityjson_lib::cityjson_types::resources::handles::MaterialHandle;
use cityjson_lib::cityjson_types::resources::mapping::MaterialMap;
use cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage;
use cityjson_lib::cityjson_types::v2_0::appearance::material::Material;
use cityjson_lib::cityjson_types::v2_0::appearance::{ThemeName, RGB};
use cityjson_lib::cityjson_types::v2_0::geometry::{
    build_surface_material_map, Geometry, GeometryType, StoredGeometryParts,
};
use cityjson_lib::cityjson_types::v2_0::CityModel as TypedCityModel;

const COLOR_THEME_NAME: &str = "color";

pub fn apply_cityobject_colors(
    model: &mut cityjson_lib::CityModel,
    object_colors: &BTreeMap<String, RGB>,
) -> Result<(), Box<dyn std::error::Error>> {
    if object_colors.is_empty() {
        return Ok(());
    }

    let cityobject_handles = model.cityobjects().ids().collect::<Vec<_>>();
    for cityobject_handle in cityobject_handles {
        let Some((object_type, geometry_handles)) =
            model
                .cityobjects()
                .get(cityobject_handle)
                .map(|cityobject| {
                    (
                        cityobject.type_cityobject().to_string(),
                        cityobject.geometry().map(|handles| handles.to_vec()),
                    )
                })
        else {
            return Err(format!(
                "missing CityObject handle {cityobject_handle} while applying city colors"
            )
            .into());
        };

        let Some(object_color) = object_colors.get(object_type.as_str()) else {
            continue;
        };
        let Some(geometry_handles) = geometry_handles else {
            continue;
        };

        let material_handle = get_or_insert_color_material(model, *object_color)?;
        for geometry_handle in geometry_handles {
            let Some(geometry) = model.get_geometry(geometry_handle) else {
                return Err(format!(
                    "missing Geometry handle {geometry_handle} while applying city colors"
                )
                .into());
            };

            if !geometry_supports_surface_materials(geometry) {
                continue;
            }

            let colored_geometry =
                apply_surface_color_to_geometry(model, geometry, material_handle)?;
            model.replace_geometry(geometry_handle, colored_geometry)?;
        }
    }

    Ok(())
}

fn apply_surface_color_to_geometry(
    model: &TypedCityModel<u32, OwnedStringStorage>,
    geometry: &Geometry<u32, OwnedStringStorage>,
    material_handle: MaterialHandle,
) -> Result<Geometry<u32, OwnedStringStorage>, Box<dyn std::error::Error>> {
    let parts = geometry.clone_stored_parts();
    let material_map = build_surface_material_map(model, geometry, |_| Some(material_handle))?;

    let mut materials = parts.materials.unwrap_or_default();
    replace_or_insert_theme(
        &mut materials,
        ThemeName::new(COLOR_THEME_NAME.to_string()),
        material_map,
    );

    Ok(Geometry::from_stored_parts(StoredGeometryParts {
        materials: Some(materials),
        ..parts
    }))
}

fn get_or_insert_color_material(
    model: &mut cityjson_lib::CityModel,
    color: RGB,
) -> Result<MaterialHandle, Box<dyn std::error::Error>> {
    let mut material = Material::new(COLOR_THEME_NAME.to_string());
    material.set_diffuse_color(Some(color));
    Ok(model.get_or_insert_material(material)?)
}

fn replace_or_insert_theme(
    themes: &mut Vec<(ThemeName<OwnedStringStorage>, MaterialMap<u32>)>,
    theme: ThemeName<OwnedStringStorage>,
    map: MaterialMap<u32>,
) {
    if let Some(existing) = themes
        .iter_mut()
        .find(|(existing_theme, _)| *existing_theme == theme)
    {
        *existing = (theme, map);
    } else {
        themes.push((theme, map));
    }
}

fn geometry_supports_surface_materials(geometry: &Geometry<u32, OwnedStringStorage>) -> bool {
    matches!(
        *geometry.type_geometry(),
        GeometryType::MultiSurface
            | GeometryType::CompositeSurface
            | GeometryType::Solid
            | GeometryType::MultiSolid
            | GeometryType::CompositeSolid
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cityjson_lib::cityjson_types::v2_0::boundary::nested::BoundaryNestedMultiOrCompositeSurface32;
    use cityjson_lib::cityjson_types::v2_0::{
        Boundary32, CityObject, CityObjectIdentifier, CityObjectType, GeometryType,
        RealWorldCoordinate,
    };
    use cityjson_lib::cityjson_types::CityModelType;
    use serde_json::Value;

    fn sample_surface_boundary() -> Boundary32 {
        let nested: BoundaryNestedMultiOrCompositeSurface32 = vec![vec![vec![0, 1, 2]]];
        nested.try_into().unwrap()
    }

    fn sample_model() -> cityjson_lib::CityModel {
        let mut model = cityjson_lib::CityModel::new(CityModelType::CityJSON);
        model
            .add_vertex(RealWorldCoordinate::new(0.0, 0.0, 0.0))
            .unwrap();
        model
            .add_vertex(RealWorldCoordinate::new(1.0, 0.0, 0.0))
            .unwrap();
        model
            .add_vertex(RealWorldCoordinate::new(0.0, 1.0, 0.0))
            .unwrap();

        let geometry = Geometry::from_stored_parts(StoredGeometryParts {
            type_geometry: GeometryType::MultiSurface,
            lod: None,
            boundaries: Some(sample_surface_boundary()),
            semantics: None,
            materials: None,
            textures: None,
            instance: None,
        });
        let geometry_handle = model.add_geometry(geometry).unwrap();

        let mut cityobject = CityObject::new(
            CityObjectIdentifier::new("building-1".to_string()),
            CityObjectType::Building,
        );
        cityobject.add_geometry(geometry_handle);
        model.cityobjects_mut().add(cityobject).unwrap();
        model
    }

    fn json_value(model: &cityjson_lib::CityModel) -> Value {
        let mut bytes = Vec::new();
        cityjson_lib::json::to_writer_with_options(
            &mut bytes,
            model,
            cityjson_lib::json::WriteOptions::default(),
        )
        .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn apply_cityobject_colors_adds_cityjson_materials() {
        let mut model = sample_model();
        let mut colors = BTreeMap::new();
        colors.insert("Building".to_string(), RGB::new(1.0, 0.0, 0.0));

        apply_cityobject_colors(&mut model, &colors).unwrap();

        let json = json_value(&model);
        assert_eq!(
            json["appearance"]["materials"][0]["name"],
            Value::String("color".to_string())
        );
        assert_eq!(
            json["appearance"]["materials"][0]["diffuseColor"],
            serde_json::json!([1.0, 0.0, 0.0])
        );
        assert!(json["CityObjects"]["building-1"]["geometry"][0]["material"]["color"].is_object());
    }
}
