#![allow(dead_code)]
use std::collections::BTreeMap;

use cityjson_lib::cityjson_types::resources::handles::{
    GeometryHandle, MaterialHandle, SemanticHandle,
};
use cityjson_lib::cityjson_types::resources::mapping::{MaterialMap, SemanticMap, TextureMap};
use cityjson_lib::cityjson_types::resources::storage::StringStorage;
use cityjson_lib::cityjson_types::v2_0::appearance::material::Material;
use cityjson_lib::cityjson_types::v2_0::appearance::{ThemeName, RGB};
use cityjson_lib::cityjson_types::v2_0::geometry::{
    Geometry, GeometryType, StoredGeometryInstance, StoredGeometryParts,
};
use cityjson_lib::cityjson_types::v2_0::vertex::VertexRef;

const COLOR_THEME_NAME: &str = "color";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrimitiveFamily {
    Point,
    Linestring,
    Surface,
}

pub fn apply_cityobject_colors(
    model: &mut cityjson_lib::CityModel,
    object_colors: &BTreeMap<String, RGB>,
) -> Result<(), Box<dyn std::error::Error>> {
    if object_colors.is_empty() {
        return Ok(());
    }

    let handles = model.cityobjects().ids().collect::<Vec<_>>();
    for cityobject_handle in handles {
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
        let mut replacements = BTreeMap::<GeometryHandle, GeometryHandle>::new();
        for geometry_handle in &geometry_handles {
            let Some(geometry) = model.get_geometry(*geometry_handle) else {
                return Err(format!(
                    "missing Geometry handle {geometry_handle} while applying city colors"
                )
                .into());
            };

            if !geometry_supports_surface_materials(geometry) {
                continue;
            }

            let colored_geometry = apply_surface_color_to_geometry(geometry, material_handle)?;
            let new_handle = model.add_geometry(colored_geometry)?;
            replacements.insert(*geometry_handle, new_handle);
        }

        if replacements.is_empty() {
            continue;
        }

        let cityobject = model
            .cityobjects_mut()
            .get_mut(cityobject_handle)
            .ok_or_else(|| {
                format!(
                    "missing CityObject handle {cityobject_handle} while updating geometry references"
                )
            })?;
        cityobject.clear_geometry();
        for geometry_handle in geometry_handles {
            let replacement = replacements
                .get(&geometry_handle)
                .copied()
                .unwrap_or(geometry_handle);
            cityobject.add_geometry(replacement);
        }
    }

    Ok(())
}

pub fn apply_cityobject_colors_from_pairs(
    model: &mut cityjson_lib::CityModel,
    colors: &[(String, RGB)],
) -> Result<(), Box<dyn std::error::Error>> {
    let object_colors = colors.iter().cloned().collect::<BTreeMap<_, _>>();
    apply_cityobject_colors(model, &object_colors)
}

pub fn build_surface_material_map<VR, F>(
    geometry: &Geometry<VR, impl StringStorage>,
    mut assign: F,
) -> Result<MaterialMap<VR>, Box<dyn std::error::Error>>
where
    VR: VertexRef,
    F: FnMut(usize) -> Option<MaterialHandle>,
{
    ensure_surface_geometry(geometry)?;
    let boundary = geometry.boundaries().ok_or_else(|| {
        format!(
            "geometry '{}' is missing boundaries",
            geometry.type_geometry()
        )
    })?;

    let mut map = MaterialMap::new();
    for index in 0..boundary.surfaces().len() {
        map.add_surface(assign(index));
    }
    Ok(map)
}

pub fn build_point_semantic_map<VR, F>(
    geometry: &Geometry<VR, impl StringStorage>,
    mut assign: F,
) -> Result<SemanticMap<VR>, Box<dyn std::error::Error>>
where
    VR: VertexRef,
    F: FnMut(usize) -> Option<SemanticHandle>,
{
    ensure_geometry_family(geometry, PrimitiveFamily::Point)?;
    let boundary = geometry.boundaries().ok_or_else(|| {
        format!(
            "geometry '{}' is missing boundaries",
            geometry.type_geometry()
        )
    })?;

    let mut map = SemanticMap::new();
    for index in 0..boundary.vertices().len() {
        map.add_point(assign(index));
    }
    Ok(map)
}

pub fn build_linestring_semantic_map<VR, F>(
    geometry: &Geometry<VR, impl StringStorage>,
    mut assign: F,
) -> Result<SemanticMap<VR>, Box<dyn std::error::Error>>
where
    VR: VertexRef,
    F: FnMut(usize) -> Option<SemanticHandle>,
{
    ensure_geometry_family(geometry, PrimitiveFamily::Linestring)?;
    let boundary = geometry.boundaries().ok_or_else(|| {
        format!(
            "geometry '{}' is missing boundaries",
            geometry.type_geometry()
        )
    })?;

    let mut map = SemanticMap::new();
    for index in 0..boundary.rings().len() {
        map.add_linestring(assign(index));
    }
    Ok(map)
}

pub fn build_surface_semantic_map<VR, F>(
    geometry: &Geometry<VR, impl StringStorage>,
    mut assign: F,
) -> Result<SemanticMap<VR>, Box<dyn std::error::Error>>
where
    VR: VertexRef,
    F: FnMut(usize) -> Option<SemanticHandle>,
{
    ensure_surface_geometry(geometry)?;
    let boundary = geometry.boundaries().ok_or_else(|| {
        format!(
            "geometry '{}' is missing boundaries",
            geometry.type_geometry()
        )
    })?;

    let mut map = SemanticMap::new();
    for index in 0..boundary.surfaces().len() {
        map.add_surface(assign(index));
    }
    Ok(map)
}

pub fn clone_stored_parts<VR, SS>(geometry: &Geometry<VR, SS>) -> StoredGeometryParts<VR, SS>
where
    VR: VertexRef,
    SS: StringStorage,
{
    StoredGeometryParts {
        type_geometry: *geometry.type_geometry(),
        lod: geometry.lod().copied(),
        boundaries: geometry.boundaries().cloned(),
        semantics: geometry.semantics().map(|semantics| {
            let mut map = SemanticMap::new();
            for resource in semantics.points() {
                map.add_point(*resource);
            }
            for resource in semantics.linestrings() {
                map.add_linestring(*resource);
            }
            for resource in semantics.surfaces() {
                map.add_surface(*resource);
            }
            map
        }),
        materials: geometry.materials().map(|themes| {
            themes
                .iter()
                .map(|(theme, assignments)| {
                    let mut map = MaterialMap::new();
                    for resource in assignments.points() {
                        map.add_point(*resource);
                    }
                    for resource in assignments.linestrings() {
                        map.add_linestring(*resource);
                    }
                    for resource in assignments.surfaces() {
                        map.add_surface(*resource);
                    }
                    (theme.clone(), map)
                })
                .collect()
        }),
        textures: geometry.textures().map(|themes| {
            themes
                .iter()
                .map(|(theme, texture_map)| {
                    let mut map = TextureMap::new();
                    for vertex in texture_map.vertices() {
                        map.add_vertex(*vertex);
                    }
                    for ring in texture_map.rings() {
                        map.add_ring(*ring);
                    }
                    for texture in texture_map.ring_textures() {
                        map.add_ring_texture(*texture);
                    }
                    (theme.clone(), map)
                })
                .collect()
        }),
        instance: geometry.instance().map(|instance| StoredGeometryInstance {
            template: instance.template(),
            reference_point: instance.reference_point(),
            transformation: instance.transformation(),
        }),
    }
}

pub fn clone_stored_geometry<VR, SS>(geometry: &Geometry<VR, SS>) -> Geometry<VR, SS>
where
    VR: VertexRef,
    SS: StringStorage,
{
    Geometry::from_stored_parts(clone_stored_parts(geometry))
}

fn apply_surface_color_to_geometry<VR>(
    geometry: &Geometry<VR, cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage>,
    material_handle: MaterialHandle,
) -> Result<
    Geometry<VR, cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage>,
    Box<dyn std::error::Error>,
>
where
    VR: VertexRef,
{
    let parts = clone_stored_parts(geometry);
    let material_map = build_surface_material_map(geometry, |_| Some(material_handle))?;

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

fn get_or_insert_color_material<VR>(
    model: &mut cityjson_lib::cityjson_types::v2_0::CityModel<
        VR,
        cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage,
    >,
    color: RGB,
) -> Result<MaterialHandle, Box<dyn std::error::Error>>
where
    VR: VertexRef,
{
    let mut material = Material::new(COLOR_THEME_NAME.to_string());
    material.set_diffuse_color(Some(color));
    Ok(model.get_or_insert_material(material)?)
}

fn replace_or_insert_theme<VR>(
    themes: &mut Vec<(
        ThemeName<cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage>,
        MaterialMap<VR>,
    )>,
    theme: ThemeName<cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage>,
    map: MaterialMap<VR>,
) where
    VR: VertexRef,
{
    if let Some(existing) = themes
        .iter_mut()
        .find(|(existing_theme, _)| *existing_theme == theme)
    {
        *existing = (theme, map);
    } else {
        themes.push((theme, map));
    }
}

fn geometry_supports_surface_materials<VR, SS>(geometry: &Geometry<VR, SS>) -> bool
where
    VR: VertexRef,
    SS: StringStorage,
{
    matches!(
        *geometry.type_geometry(),
        GeometryType::MultiSurface
            | GeometryType::CompositeSurface
            | GeometryType::Solid
            | GeometryType::MultiSolid
            | GeometryType::CompositeSolid
    )
}

fn ensure_surface_geometry<VR, SS>(
    geometry: &Geometry<VR, SS>,
) -> Result<(), Box<dyn std::error::Error>>
where
    VR: VertexRef,
    SS: StringStorage,
{
    if geometry.instance().is_some() {
        return Err(
            "GeometryInstance cannot carry material or semantic surface assignments".into(),
        );
    }
    if !geometry_supports_surface_materials(geometry) {
        return Err(format!(
            "geometry '{}' does not support surface assignments",
            geometry.type_geometry()
        )
        .into());
    }
    Ok(())
}

fn ensure_geometry_family<VR, SS>(
    geometry: &Geometry<VR, SS>,
    family: PrimitiveFamily,
) -> Result<(), Box<dyn std::error::Error>>
where
    VR: VertexRef,
    SS: StringStorage,
{
    if geometry.instance().is_some() {
        return Err("GeometryInstance cannot carry semantic assignments".into());
    }

    let supported = match family {
        PrimitiveFamily::Point => matches!(*geometry.type_geometry(), GeometryType::MultiPoint),
        PrimitiveFamily::Linestring => {
            matches!(*geometry.type_geometry(), GeometryType::MultiLineString)
        }
        PrimitiveFamily::Surface => geometry_supports_surface_materials(geometry),
    };

    if !supported {
        return Err(format!(
            "geometry '{}' does not support {:?} assignments",
            geometry.type_geometry(),
            family
        )
        .into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage;
    use cityjson_lib::cityjson_types::v2_0::appearance::RGB;
    use cityjson_lib::cityjson_types::v2_0::boundary::nested::BoundaryNestedMultiOrCompositeSurface32;
    use cityjson_lib::cityjson_types::v2_0::geometry::GeometryType;
    use cityjson_lib::cityjson_types::v2_0::{
        CityObject, CityObjectIdentifier, CityObjectType, Geometry, StoredGeometryParts,
    };
    use cityjson_lib::cityjson_types::CityModelType;
    use serde_json::Value;

    type Model = cityjson_lib::CityModel;

    fn sample_surface_boundary() -> cityjson_lib::cityjson_types::v2_0::boundary::Boundary32 {
        let nested: BoundaryNestedMultiOrCompositeSurface32 = vec![vec![vec![0_u32, 1, 2]]];
        nested.try_into().expect("boundary")
    }

    fn sample_geometry() -> Geometry<u32, OwnedStringStorage> {
        Geometry::from_stored_parts(StoredGeometryParts {
            type_geometry: GeometryType::MultiSurface,
            lod: None,
            boundaries: Some(sample_surface_boundary()),
            semantics: None,
            materials: None,
            textures: None,
            instance: None,
        })
    }

    fn sample_model() -> Model {
        let mut model = Model::new(CityModelType::CityJSON);
        model
            .add_vertex(cityjson_lib::cityjson_types::v2_0::RealWorldCoordinate::new(0.0, 0.0, 0.0))
            .unwrap();
        model
            .add_vertex(cityjson_lib::cityjson_types::v2_0::RealWorldCoordinate::new(1.0, 0.0, 0.0))
            .unwrap();
        model
            .add_vertex(cityjson_lib::cityjson_types::v2_0::RealWorldCoordinate::new(0.0, 1.0, 0.0))
            .unwrap();

        let geometry = sample_geometry();
        let handle = model.add_geometry(geometry).unwrap();

        let mut building = CityObject::new(
            CityObjectIdentifier::new("building-1".to_string()),
            CityObjectType::Building,
        );
        building.add_geometry(handle);
        model.cityobjects_mut().add(building).unwrap();

        model
    }

    fn model_json(model: &Model) -> Value {
        let mut buffer = Vec::new();
        cityjson_lib::json::to_writer_with_options(
            &mut buffer,
            model,
            cityjson_lib::json::WriteOptions::default(),
        )
        .expect("serialize model");
        serde_json::from_slice(&buffer).expect("valid json")
    }

    #[test]
    fn clone_stored_geometry_round_trips_json() {
        let geometry = sample_geometry();
        let cloned = clone_stored_geometry(&geometry);

        let original_json = serde_json::to_value(
            geometry
                .boundaries()
                .unwrap()
                .to_nested_multi_or_composite_surface()
                .unwrap(),
        )
        .unwrap();
        let cloned_json = serde_json::to_value(
            cloned
                .boundaries()
                .unwrap()
                .to_nested_multi_or_composite_surface()
                .unwrap(),
        )
        .unwrap();

        assert_eq!(original_json, cloned_json);
        assert_eq!(geometry.type_geometry(), cloned.type_geometry());
    }

    #[test]
    fn build_surface_material_map_rejects_multipoint_geometry() {
        let boundary: cityjson_lib::cityjson_types::v2_0::boundary::Boundary32 =
            vec![0_u32, 1, 2].into();
        let geometry: Geometry<u32, OwnedStringStorage> =
            Geometry::from_stored_parts(StoredGeometryParts {
                type_geometry: GeometryType::MultiPoint,
                lod: None,
                boundaries: Some(boundary),
                semantics: None,
                materials: None,
                textures: None,
                instance: None,
            });

        assert!(build_surface_material_map(&geometry, |_| None).is_err());
    }

    #[test]
    fn apply_cityobject_colors_adds_cityjson_materials() {
        let mut model = sample_model();
        let mut colors = BTreeMap::new();
        colors.insert("Building".to_string(), RGB::new(1.0, 0.0, 0.0));

        apply_cityobject_colors(&mut model, &colors).expect("apply colors");

        let json = model_json(&model);
        let appearance = json.get("appearance").expect("appearance section");
        let materials = appearance
            .get("materials")
            .and_then(Value::as_array)
            .expect("materials array");
        assert_eq!(materials.len(), 1);
        assert_eq!(
            materials[0].get("name").and_then(Value::as_str),
            Some("color")
        );

        let diffuse = materials[0]
            .get("diffuseColor")
            .and_then(Value::as_array)
            .expect("diffuse color");
        assert_eq!(diffuse.len(), 3);
        assert_eq!(diffuse[0].as_f64(), Some(1.0));

        let cityobjects = json
            .get("CityObjects")
            .and_then(Value::as_object)
            .expect("cityobjects map");
        let building = cityobjects.get("building-1").expect("building object");
        let geometry = building
            .get("geometry")
            .and_then(Value::as_array)
            .and_then(|entries| entries.first())
            .expect("geometry entry");
        assert_eq!(
            geometry
                .get("material")
                .and_then(|material| material.get("color"))
                .and_then(|color| color.get("value"))
                .and_then(Value::as_u64),
            Some(0)
        );
    }
}
