use std::env;
use std::path::PathBuf;

use cityjson_convert::{
    tabulate_addresses, tabulate_cityobjects, tabulate_model_metadata,
    tabulate_semantic_assignments, tabulate_semantics, ColumnOrigin, LogicalType, PrimitiveType,
    TableSchema, Value,
};
use cityjson_lib::cityjson_types::v2_0::OwnedAttributeValue;
use cityjson_lib::json;

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Locates the complete `CityJSON` 2.0 conformance fixture used by the public
/// table test.
///
/// The corpus checkout is intentionally external to this crate because the
/// fixture is shared across `CityJSON` implementations. `CITYJSON_CORPUS_DIR` can
/// override the default sibling-checkout location.
fn corpus_fixture() -> Option<PathBuf> {
    let corpus_root = env::var_os("CITYJSON_CORPUS_DIR").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("cityjson-corpus")
        },
        PathBuf::from,
    );
    let fixture = corpus_root
        .join("cases")
        .join("conformance")
        .join("v2_0")
        .join("cityjson_fake_complete")
        .join("cityjson_fake_complete.city.json");

    fixture.is_file().then_some(fixture)
}

/// Returns the schema index for a named dynamic column used by a test assertion.
fn column_index(table: &cityjson_convert::CityObjectTable<'_>, name: &str) -> usize {
    table
        .schema()
        .columns
        .iter()
        .position(|column| column.name == name)
        .unwrap_or_else(|| panic!("missing column {name}"))
}

fn schema_column_index(schema: &TableSchema<'_>, name: &str) -> usize {
    schema
        .columns
        .iter()
        .position(|column| column.name == name)
        .unwrap_or_else(|| panic!("missing column {name}"))
}

/// Traverses a value completely so the corpus test exercises lazy
/// lists and structs in addition to top-level scalar cells.
fn validate_value(cell: Value<'_, '_>) {
    match cell {
        Value::List(values) => {
            for value in values.iter() {
                validate_value(value.expect("list item should conform to the inferred schema"));
            }
        }
        Value::Struct(values) => {
            for field in values.fields() {
                let (_, value) = field.expect("struct field should conform to the inferred schema");
                validate_value(value);
            }
        }
        Value::Null
        | Value::Boolean(_)
        | Value::UInt64(_)
        | Value::Int64(_)
        | Value::Float64(_)
        | Value::Utf8(_)
        | Value::GeometryRef(_)
        | Value::Json(_) => {}
    }
}

/// Tabulates the complete `CityJSON` 2.0 conformance fixture and consumes every
/// row and value.
///
/// Input: `cityjson_fake_complete`, which contains four `CityObjects` and covers
/// fixed fields, attributes, custom members, nested values, lists, nulls,
/// addresses, and stored geographical extents.
///
/// Assertions: tabulation succeeds; row count, order, and identifiers match the
/// source model; every row has one resolvable value per schema column; all lazy
/// nested values can be traversed; and representative fixed and dynamic values
/// reach the expected row. If the corpus checkout is absent, the test is skipped
/// instead of failing the suite.
///
/// Invariants protected: one row per `CityObject` in model order, positional
/// schema/value alignment, and end-to-end convertibility of the complete fixture.
#[test]
fn tabulates_cityjson_fake_complete() {
    let Some(fixture) = corpus_fixture() else {
        eprintln!(
            "skipping tabulates_cityjson_fake_complete: set CITYJSON_CORPUS_DIR or checkout cityjson-corpus next to the workspace"
        );
        return;
    };

    let model = json::from_file(fixture).expect("load cityjson_fake_complete fixture");
    let table = tabulate_cityobjects(&model).expect("tabulate CityObjects");
    let rows = table.rows().collect::<Vec<_>>();

    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter().map(|row| row.cityobject_id).collect::<Vec<_>>(),
        ["id-1", "id-3", "a-tree", "my-neighbourhood"]
    );
    assert_eq!(
        rows.iter().map(|row| row.cityobject_ix).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(rows[0].cityobject_type_name().to_string(), "BuildingPart");
    assert_eq!(rows[1].cityobject_type_name().to_string(), "+NoiseBuilding");

    let height = column_index(&table, "attributes__measuredHeight");
    let height_schema = &table.schema().columns[height];
    assert_eq!(height_schema.origin, ColumnOrigin::Attributes);
    assert_eq!(height_schema.logical_type, LogicalType::Float64);
    assert!(height_schema.nullable);

    let children_roles = column_index(&table, "children_roles");
    let children_roles_schema = &table.schema().columns[children_roles];
    assert_eq!(children_roles_schema.origin, ColumnOrigin::Extra);
    assert!(matches!(
        children_roles_schema.logical_type,
        LogicalType::List { ref item, .. } if matches!(item.as_ref(), LogicalType::Utf8)
    ));

    for row in &rows {
        let cells = row.values().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(cells.len(), table.schema().columns.len());
        for cell in cells {
            validate_value(cell);
        }
    }

    assert_eq!(
        rows[0].bbox,
        Some([84710.1, 446_846.0, -5.3, 84757.1, 446_944.0, 40.9])
    );
    assert!(matches!(
        rows[0].value(height).unwrap().unwrap(),
        Value::Float64(22.3)
    ));
    assert!(matches!(
        rows[2].value(height).unwrap().unwrap(),
        Value::Null
    ));
    assert!(rows[0].value(table.schema().columns.len()).is_none());
}

/// Establishes the borrowed and lazy API contract with a minimal inline model.
///
/// Input: one Building with a string, a heterogeneous JSON-fallback list, and a
/// homogeneous list of structs.
///
/// Assertions: schema path segments and scalar/JSON cells point at source model
/// storage, while list and struct children are exposed through lazy iterators.
///
/// Invariants protected: table values borrow source data instead of cloning
/// it, and nested typed values can be consumed without owned child collections.
#[test]
fn borrows_source_values_and_traverses_nested_values_lazily() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "attributes":{
                        "name":"Library",
                        "mixed":[1,false],
                        "items":[
                            {"label":"first","count":1},
                            {"label":"second"}
                        ]
                    }
                }
            },
            "vertices":[]
        }"#,
    )
    .expect("parse inline CityJSON");
    let (_, object) = model.cityobjects().first().unwrap();
    let attributes = object.attributes().unwrap();
    let table = tabulate_cityobjects(&model).unwrap();

    let name_ix = column_index(&table, "attributes__name");
    let mixed_ix = column_index(&table, "attributes__mixed");
    let items_ix = column_index(&table, "attributes__items");
    assert!(table
        .schema()
        .columns
        .iter()
        .all(|column| !matches!(column.logical_type, LogicalType::Struct(_))));
    assert!(matches!(
        &table.schema().columns[items_ix].logical_type,
        LogicalType::List {
            item,
            ..
        } if matches!(item.as_ref(), LogicalType::Struct(_))
    ));
    let source_name_key = attributes
        .keys()
        .find(|name| name.as_str() == "name")
        .unwrap();
    assert!(std::ptr::eq(
        table.schema().columns[name_ix].path[0],
        source_name_key.as_str()
    ));

    let row = table.rows().next().unwrap();
    let Value::Utf8(name) = row.value(name_ix).unwrap().unwrap() else {
        panic!("name should be a borrowed UTF-8 value");
    };
    let cityjson_lib::cityjson_types::v2_0::OwnedAttributeValue::String(source_name) =
        attributes.get("name").unwrap()
    else {
        unreachable!();
    };
    assert!(std::ptr::eq(name, source_name.as_str()));

    let Value::Json(mixed) = row.value(mixed_ix).unwrap().unwrap() else {
        panic!("mixed should be a borrowed JSON fallback value");
    };
    assert!(std::ptr::eq(mixed, attributes.get("mixed").unwrap()));

    let Value::List(items) = row.value(items_ix).unwrap().unwrap() else {
        panic!("items should be a lazy list");
    };
    let Value::Struct(first_item) = items.iter().next().unwrap().unwrap() else {
        panic!("list item should be a lazy struct");
    };
    let mut fields = first_item.fields();
    let (count_name, count) = fields.next().unwrap().unwrap();
    let (label_name, label) = fields.next().unwrap().unwrap();
    assert_eq!(count_name, "count");
    assert!(matches!(count, Value::UInt64(1)));
    assert_eq!(label_name, "label");
    assert!(matches!(label, Value::Utf8("first")));
    assert!(fields.next().is_none());
}

#[test]
fn tabulates_cityobject_extra_without_prefix_and_resolves_collisions() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "attributes":{"name":"Library"},
                    "name":"extra-name",
                    "cityobject_id":"extra-id"
                }
            },
            "vertices":[]
        }"#,
    )
    .expect("parse extra CityJSON");
    let table = tabulate_cityobjects(&model).unwrap();

    let extra_name = column_index(&table, "name");
    let fixed_conflict = column_index(&table, "cityobject_id__2");
    assert_eq!(
        table.schema().columns[extra_name].origin,
        ColumnOrigin::Extra
    );
    assert_eq!(
        table.schema().columns[fixed_conflict].origin,
        ColumnOrigin::Extra
    );

    let row = table.rows().next().unwrap();
    assert!(matches!(
        row.value(extra_name).unwrap().unwrap(),
        Value::Utf8("extra-name")
    ));
    assert!(matches!(
        row.value(fixed_conflict).unwrap().unwrap(),
        Value::Utf8("extra-id")
    ));
}

#[test]
fn exposes_cityobject_hierarchy_as_resolved_id_lists() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{"type":"Building","children":["room"]},
                "room":{"type":"BuildingRoom","parents":["building"]}
            },
            "vertices":[]
        }"#,
    )
    .expect("parse hierarchy CityJSON");
    let table = tabulate_cityobjects(&model).unwrap();
    let rows = table.rows().collect::<Vec<_>>();

    assert_eq!(rows[0].children().unwrap().ids(), &["room"]);
    assert_eq!(rows[0].parents().unwrap().ids(), &[] as &[&str]);
    assert_eq!(rows[1].parents().unwrap().ids(), &["building"]);
    assert_eq!(rows[1].children().unwrap().ids(), &[] as &[&str]);
}

#[test]
fn tabulates_model_metadata_with_extent_wkt_and_extra_schema() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{},
            "vertices":[],
            "metadata":{
                "identifier":"dataset-1",
                "referenceDate":"2026-01-02",
                "referenceSystem":"https://www.opengis.net/def/crs/EPSG/0/7415",
                "title":"Demo dataset",
                "geographicalExtent":[1.0,2.0,3.0,4.0,5.0,6.0],
                "pointOfContact":{
                    "contactName":"Ada",
                    "emailAddress":"ada@example.test",
                    "role":"pointOfContact",
                    "website":"https://example.test",
                    "contactType":"individual",
                    "phone":"+31000000000",
                    "organization":"Example"
                },
                "+quality":{"score":7}
            }
        }"#,
    )
    .expect("parse metadata CityJSON");
    let table = tabulate_model_metadata(&model).unwrap();
    let rows = table.rows().collect::<Vec<_>>();

    assert_eq!(rows.len(), 1);
    let fixed = rows[0].fixed();
    assert_eq!(fixed.identifier.as_deref(), Some("dataset-1"));
    assert_eq!(fixed.reference_date.as_deref(), Some("2026-01-02"));
    assert_eq!(fixed.title.as_deref(), Some("Demo dataset"));
    assert_eq!(
        fixed.geographical_extent,
        Some([1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
    );
    assert_eq!(
        fixed.geographical_extent_wkt.as_deref(),
        Some("POLYGON((1 2, 4 2, 4 5, 1 5, 1 2))")
    );
    assert_eq!(fixed.contact_name.as_deref(), Some("Ada"));

    let score = schema_column_index(table.schema(), "metadata_extra__+quality__score");
    assert_eq!(
        table.schema().columns[score].origin,
        ColumnOrigin::MetadataExtra
    );
    assert!(matches!(
        rows[0].value(score).unwrap().unwrap(),
        Value::UInt64(7)
    ));
}

#[test]
fn tabulates_semantic_definitions_with_attributes_and_relationships() {
    let model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"2.0",
                        "boundaries":[[[0,1,2]]],
                        "semantics":{
                            "surfaces":[
                                {"type":"RoofSurface","children":[1],"slope":30},
                                {"type":"WallSurface","parent":0,"material":"brick"}
                            ],
                            "values":[0]
                        }
                    }]
                }
            },
            "vertices":[[0,0,0],[1,0,0],[0,1,0]]
        }"#,
    )
    .expect("parse semantic CityJSON");
    let table = tabulate_semantics(&model).unwrap();
    let rows = table.rows().collect::<Vec<_>>();

    assert_eq!(rows.len(), 2);
    let first = rows[0].fixed();
    let second = rows[1].fixed();
    assert_eq!(first.semantic_type_name().to_string(), "RoofSurface");
    assert_eq!(second.semantic_type_name().to_string(), "WallSurface");
    assert_eq!(first.children, vec![second.semantic_id]);
    assert_eq!(second.parent, Some(first.semantic_id));

    let slope = schema_column_index(table.schema(), "attributes__slope");
    assert_eq!(
        table.schema().columns[slope].origin,
        ColumnOrigin::SemanticAttributes
    );
    assert!(matches!(
        rows[0].value(slope).unwrap().unwrap(),
        Value::UInt64(30)
    ));
    assert!(matches!(
        rows[1].value(slope).unwrap().unwrap(),
        Value::Null
    ));
}

fn with_semantic_assignment_rows(
    input: &[u8],
    check: impl FnOnce(Vec<cityjson_convert::SemanticAssignmentRow<'_>>),
) {
    let model = json::from_slice(input).expect("parse semantic assignment fixture");
    let table = tabulate_semantic_assignments(&model).expect("tabulate semantic assignments");
    check(table.rows().cloned().collect());
}

#[test]
fn tabulates_point_and_linestring_semantic_assignments() {
    with_semantic_assignment_rows(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "points":{
                    "type":"GenericCityObject",
                    "geometry":[{
                        "type":"MultiPoint",
                        "lod":"1",
                        "boundaries":[0,1],
                        "semantics":{
                            "surfaces":[{"type":"RoofSurface"}],
                            "values":[0,null]
                        }
                    }]
                }
            },
            "vertices":[[0,0,0],[1,0,0]]
        }"#,
        |point_rows| {
            assert_eq!(point_rows.len(), 2);
            assert_eq!(point_rows[0].primitive_type, PrimitiveType::Point);
            assert_eq!(point_rows[0].primitive_ix, 0);
            assert_eq!(point_rows[0].point_ix, Some(0));
            assert_eq!(point_rows[0].semantic_id, Some(0));
            assert_eq!(point_rows[1].primitive_ix, 1);
            assert_eq!(point_rows[1].point_ix, Some(1));
            assert_eq!(point_rows[1].semantic_id, None);
        },
    );

    with_semantic_assignment_rows(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "lines":{
                    "type":"GenericCityObject",
                    "geometry":[{
                        "type":"MultiLineString",
                        "lod":"1",
                        "boundaries":[[0,1],[1,2]],
                        "semantics":{
                            "surfaces":[{"type":"TrafficArea"}],
                            "values":[0,null]
                        }
                    }]
                }
            },
            "vertices":[[0,0,0],[1,0,0],[2,0,0]]
        }"#,
        |line_rows| {
            assert_eq!(line_rows.len(), 2);
            assert_eq!(line_rows[0].primitive_type, PrimitiveType::LineString);
            assert_eq!(line_rows[0].primitive_ix, 0);
            assert_eq!(line_rows[0].linestring_ix, Some(0));
            assert_eq!(line_rows[0].semantic_id, Some(0));
            assert_eq!(line_rows[1].primitive_ix, 1);
            assert_eq!(line_rows[1].linestring_ix, Some(1));
            assert_eq!(line_rows[1].semantic_id, None);
        },
    );
}

#[test]
fn tabulates_surface_semantic_assignments_with_structural_paths() {
    with_semantic_assignment_rows(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "solid":{
                    "type":"Building",
                    "geometry":[{
                        "type":"Solid",
                        "lod":"2",
                        "boundaries":[[
                            [[0,1,2]],
                            [[0,2,3]]
                        ],[
                            [[4,5,6]]
                        ]],
                        "semantics":{
                            "surfaces":[
                                {"type":"RoofSurface"},
                                {"type":"WallSurface"}
                            ],
                            "values":[[0,1],[null]]
                        }
                    }]
                }
            },
            "vertices":[
                [0,0,0],[1,0,0],[0,1,0],[1,1,0],
                [0,0,1],[1,0,1],[0,1,1]
            ]
        }"#,
        |rows| {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].primitive_type, PrimitiveType::Surface);
            assert_eq!(rows[0].primitive_ix, 0);
            assert_eq!(rows[0].solid_ix, None);
            assert_eq!(rows[0].shell_ix, Some(0));
            assert_eq!(rows[0].surface_ix, Some(0));
            assert_eq!(rows[0].semantic_id, Some(0));
            assert_eq!(rows[1].primitive_ix, 1);
            assert_eq!(rows[1].shell_ix, Some(0));
            assert_eq!(rows[1].surface_ix, Some(1));
            assert_eq!(rows[1].semantic_id, Some(1));
            assert_eq!(rows[2].primitive_ix, 2);
            assert_eq!(rows[2].shell_ix, Some(1));
            assert_eq!(rows[2].surface_ix, Some(0));
            assert_eq!(rows[2].semantic_id, None);
        },
    );
}

#[test]
fn tabulates_multisolid_semantic_assignments_with_solid_paths() {
    with_semantic_assignment_rows(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "multi-solid":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiSolid",
                        "lod":"2",
                        "boundaries":[
                            [[[[0,1,2]]]],
                            [[[[3,4,5]],[[3,5,6]]]]
                        ],
                        "semantics":{
                            "surfaces":[{"type":"RoofSurface"}],
                            "values":[[[0]],[[null,0]]]
                        }
                    }]
                }
            },
            "vertices":[
                [0,0,0],[1,0,0],[0,1,0],[2,0,0],[3,0,0],[2,1,0],[3,1,0]
            ]
        }"#,
        |rows| {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].primitive_ix, 0);
            assert_eq!(rows[0].solid_ix, Some(0));
            assert_eq!(rows[0].shell_ix, Some(0));
            assert_eq!(rows[0].surface_ix, Some(0));
            assert_eq!(rows[0].semantic_id, Some(0));
            assert_eq!(rows[1].primitive_ix, 1);
            assert_eq!(rows[1].solid_ix, Some(1));
            assert_eq!(rows[1].shell_ix, Some(0));
            assert_eq!(rows[1].surface_ix, Some(0));
            assert_eq!(rows[1].semantic_id, None);
            assert_eq!(rows[2].primitive_ix, 2);
            assert_eq!(rows[2].solid_ix, Some(1));
            assert_eq!(rows[2].shell_ix, Some(0));
            assert_eq!(rows[2].surface_ix, Some(1));
            assert_eq!(rows[2].semantic_id, Some(0));
        },
    );
}

#[test]
fn skips_geometries_without_semantic_maps() {
    with_semantic_assignment_rows(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"1",
                        "boundaries":[[[0,1,2]]]
                    }]
                }
            },
            "vertices":[[0,0,0],[1,0,0],[0,1,0]]
        }"#,
        |rows| assert!(rows.is_empty()),
    );
}

#[test]
fn tabulates_resolved_geometry_instance_semantic_assignments() {
    with_semantic_assignment_rows(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "tree":{
                    "type":"SolitaryVegetationObject",
                    "geometry":[{
                        "type":"GeometryInstance",
                        "template":0,
                        "boundaries":[0],
                        "transformationMatrix":[1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1]
                    }]
                }
            },
            "vertices":[[10,10,0]],
            "geometry-templates":{
                "templates":[{
                    "type":"MultiSurface",
                    "lod":"1",
                    "boundaries":[[[0,1,2]]],
                    "semantics":{
                        "surfaces":[{"type":"RoofSurface"}],
                        "values":[0]
                    }
                }],
                "vertices-templates":[[0,0,0],[1,0,0],[0,1,0]]
            }
        }"#,
        |rows| {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].cityobject_id, "tree");
            assert!(rows[0].geometry_is_instance);
            assert_eq!(rows[0].primitive_type, PrimitiveType::Surface);
            assert_eq!(rows[0].primitive_ix, 0);
            assert_eq!(rows[0].surface_ix, Some(0));
            assert_eq!(rows[0].semantic_id, Some(0));
        },
    );
}

#[test]
fn omits_geometry_ref_attributes_from_cityobject_schema() {
    let mut model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiSurface",
                        "lod":"1",
                        "boundaries":[[[0,1,2,0]]]
                    }]
                }
            },
            "vertices":[[0,0,0],[1,0,0],[0,1,0]]
        }"#,
    )
    .expect("parse inline CityJSON");
    let geometry_handle = model
        .cityobjects()
        .iter()
        .next()
        .and_then(|(_, object)| object.geometry())
        .and_then(|geometries| geometries.first().copied())
        .expect("geometry handle");
    let (_, cityobject) = model
        .cityobjects_mut()
        .iter_mut()
        .next()
        .expect("cityobject");
    cityobject.attributes_mut().insert(
        "location".to_string(),
        OwnedAttributeValue::Geometry(geometry_handle),
    );

    let table = tabulate_cityobjects(&model).expect("tabulate CityObjects");

    assert!(table
        .schema()
        .columns
        .iter()
        .all(|column| column.name != "attributes__location"));
}

#[test]
fn tabulates_address_location_separately_from_dynamic_address_columns() {
    let mut model = json::from_slice(
        br#"{
            "type":"CityJSON",
            "version":"2.0",
            "CityObjects":{
                "building":{
                    "type":"Building",
                    "geometry":[{
                        "type":"MultiPoint",
                        "lod":"1",
                        "boundaries":[0]
                    }]
                }
            },
            "vertices":[[0,0,0]]
        }"#,
    )
    .expect("parse inline CityJSON");
    let geometry_handle = model
        .cityobjects()
        .iter()
        .next()
        .and_then(|(_, object)| object.geometry())
        .and_then(|geometries| geometries.first().copied())
        .expect("geometry handle");
    let (_, cityobject) = model
        .cityobjects_mut()
        .iter_mut()
        .next()
        .expect("cityobject");
    cityobject.extra_mut().insert(
        "address".to_string(),
        OwnedAttributeValue::Map(std::collections::HashMap::from([
            (
                "location".to_string(),
                OwnedAttributeValue::Geometry(geometry_handle),
            ),
            (
                "street".to_string(),
                OwnedAttributeValue::String("Main Street".to_string()),
            ),
        ])),
    );

    let expected_wkb =
        cityjson_convert::tabular::geometry_ref_to_multipoint_wkb(&model, geometry_handle)
            .expect("encode address location as WKB");
    let table = tabulate_addresses(&model).expect("tabulate addresses");
    let rows = table.rows().collect::<Vec<_>>();
    let street_column = schema_column_index(table.schema(), "street");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].fixed().location().unwrap(), Some(geometry_handle));
    assert_eq!(
        cityjson_convert::tabular::geometry_ref_to_multipoint_wkb(table.model(), geometry_handle)
            .unwrap(),
        expected_wkb
    );
    assert!(matches!(
        rows[0].value(street_column).unwrap().unwrap(),
        Value::Utf8("Main Street")
    ));
    assert!(table
        .schema()
        .columns
        .iter()
        .all(|column| column.name != "location"));
    assert_ne!(bytes_to_hex(&expected_wkb), "0");
}
