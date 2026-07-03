use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};
use cityjson_lib::cityjson_types::resources::storage::OwnedStringStorage;
use cityjson_lib::cityjson_types::v2_0::{
    Boundary, GeometryType, Metadata, OwnedAttributeValue, VertexIndex,
};
use cityjson_lib::CityModel;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, Transaction};
use serde_json::Map;

use crate::{
    tabulate_cityobjects, tabulate_model_metadata, tabulate_semantic_assignments,
    tabulate_semantics, CityObjectRow, CityObjectTable, LogicalType, MetadataTable,
    SemanticAssignmentTable, SemanticTable,
    Value,
};

const GPKG_APPLICATION_ID: i32 = 0x4750_4b47;
const GPKG_USER_VERSION: i32 = 10300;
const GPB_MAGIC: &[u8; 2] = b"GP";
const GPKG_GEOM_COLUMN_NAME: &str = "geom";
const GPKG_METADATA_STANDARD_URI: &str = "https://www.geopackage.org/spec/#extension_metadata";
const GPKG_METADATA_REFERENCE_SCOPE: &str = "dataset";
const SQLITE_LAST_CHANGE_SQL: &str = "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct GpkgExportOptions {
    pub split_lod: bool,
    pub split_semantics: bool,
    pub include_metadata: bool,
    pub output_crs: Option<String>,
}

impl Default for GpkgExportOptions {
    fn default() -> Self {
        Self {
            split_lod: false,
            split_semantics: false,
            include_metadata: false,
            output_crs: None,
        }
    }
}

#[derive(Debug)]
struct FeatureLayerState {
    insert_sql: String,
    extent: Option<[f64; 6]>,
}

/// Converts a `CityJSON` model to a GeoPackage file.
///
/// # Errors
///
/// Returns an error when the output directory cannot be created, metadata or
/// geometry cannot be resolved, or the GeoPackage cannot be written.
pub fn convert_to_gpkg<P: AsRef<Path>>(
    model: &CityModel,
    output: P,
    options: &GpkgExportOptions,
) -> Result<()> {
    let output = output.as_ref();
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    let cityobjects = tabulate_cityobjects(model)?;
    let cityobject_rows = cityobjects.rows().collect::<Vec<_>>();
    let metadata = if options.include_metadata {
        Some(tabulate_model_metadata(model)?)
    } else {
        None
    };
    let semantics = if options.split_semantics {
        Some((
            tabulate_semantics(model)?,
            tabulate_semantic_assignments(model)?,
        ))
    } else {
        None
    };
    let resolved_srs = resolve_srs(model, options)?;

    if output.exists() {
        fs::remove_file(output)
            .with_context(|| format!("remove existing output {}", output.display()))?;
    }

    let mut conn = Connection::open(output)
        .with_context(|| format!("open GeoPackage {}", output.display()))?;
    conn.execute_batch(&format!(
        "PRAGMA application_id = {GPKG_APPLICATION_ID};\nPRAGMA user_version = {GPKG_USER_VERSION};\nPRAGMA foreign_keys = ON;\nPRAGMA journal_mode = OFF;\nPRAGMA synchronous = OFF;"
    ))?;
    let tx = conn.transaction()?;
    let last_change: String = tx.query_row(SQLITE_LAST_CHANGE_SQL, [], |row| row.get(0))?;

    create_core_tables(&tx)?;
    insert_standard_spatial_ref_systems(&tx, resolved_srs.srs_id, &resolved_srs.label)?;

    let mut used_table_names = HashSet::new();
    let mut feature_layers: BTreeMap<String, FeatureLayerState> = BTreeMap::new();
    let mut relations = BTreeSet::new();

    for (cityobject_ix, (_, cityobject)) in model.cityobjects().iter().enumerate() {
        let row = &cityobject_rows[cityobject_ix];
        collect_cityobject_relations(row, &mut relations)?;

        let Some(geometry_handles) = cityobject.geometry() else {
            continue;
        };

        for geometry_handle in geometry_handles.iter().copied() {
            let geometry = model.resolve_geometry(geometry_handle)?;
            let Some(boundary) = geometry.boundaries() else {
                continue;
            };

            let encoded = encode_geometry_blob(model, geometry.type_geometry(), boundary, resolved_srs.srs_id)?;
            let layer_table_name = layer_table_name(
                row.cityobject_type_name().to_string(),
                geometry.type_geometry().to_string(),
                geometry.lod().map(ToString::to_string),
                options.split_lod,
                &mut used_table_names,
            );
            if !feature_layers.contains_key(&layer_table_name) {
                create_feature_table(
                    &tx,
                    &layer_table_name,
                    &cityobjects,
                    resolved_srs.srs_id,
                    geometry.type_geometry(),
                    &last_change,
                )?;
                feature_layers.insert(
                    layer_table_name.clone(),
                    FeatureLayerState {
                        insert_sql: feature_insert_sql(&layer_table_name, cityobjects.schema()),
                        extent: None,
                    },
                );
            }

            let state = feature_layers.get_mut(&layer_table_name).expect("layer exists");
            state.extent = union_bbox(state.extent, encoded.envelope);
            insert_feature_row(
                &tx,
                &state.insert_sql,
                row,
                geometry.type_geometry(),
                geometry.lod().map(ToString::to_string),
                encoded.blob,
            )?;
        }
    }

    create_relation_table(&tx)?;
    insert_cityobject_relations(&tx, &relations)?;

    if let Some((semantics_table, assignment_table)) = semantics {
        create_semantics_table(&tx, semantics_table.schema())?;
        insert_semantics_rows(&tx, &semantics_table)?;
        create_semantic_relations_table(&tx)?;
        insert_semantic_relations(&tx, &assignment_table)?;
    }

    if let Some(metadata_table) = metadata {
        create_metadata_tables(&tx)?;
        insert_metadata_rows(&tx, &metadata_table, model, &last_change)?;
    }

    update_feature_layer_extents(&tx, &feature_layers)?;

    tx.commit()?;
    Ok(())
}

#[derive(Clone, Debug)]
struct ResolvedSrs {
    srs_id: i32,
    label: String,
}

fn resolve_srs(model: &CityModel, options: &GpkgExportOptions) -> Result<ResolvedSrs> {
    if let Some(metadata) = model.metadata() {
        if let Some(reference_system) = metadata.reference_system() {
            let reference_system = reference_system.to_string();
            if let Some(srs_id) = parse_epsg_srs_id(&reference_system) {
                return Ok(ResolvedSrs {
                    srs_id,
                    label: reference_system,
                });
            }
        }
    }

    let Some(output_crs) = options.output_crs.as_ref() else {
        bail!(
            "CityJSON metadata referenceSystem is missing or ambiguous; provide --gpkg-output-crs"
        );
    };
    let Some(srs_id) = parse_epsg_srs_id(output_crs) else {
        bail!("could not parse EPSG code from --gpkg-output-crs value {output_crs:?}");
    };

    Ok(ResolvedSrs {
        srs_id,
        label: output_crs.clone(),
    })
}

fn parse_epsg_srs_id(value: &str) -> Option<i32> {
    if !value.to_ascii_uppercase().contains("EPSG") {
        return None;
    }

    let mut last_numeric = None;
    let mut current = String::new();
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            last_numeric = Some(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        last_numeric = Some(current);
    }

    last_numeric?.parse().ok()
}

fn create_core_tables(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (
            srs_name TEXT NOT NULL,
            srs_id INTEGER NOT NULL PRIMARY KEY,
            organization TEXT NOT NULL,
            organization_coordsys_id INTEGER NOT NULL,
            definition TEXT NOT NULL,
            description TEXT NOT NULL
        );
        CREATE TABLE gpkg_contents (
            table_name TEXT NOT NULL PRIMARY KEY,
            data_type TEXT NOT NULL,
            identifier TEXT UNIQUE,
            description TEXT DEFAULT '',
            last_change TEXT NOT NULL,
            min_x DOUBLE,
            min_y DOUBLE,
            max_x DOUBLE,
            max_y DOUBLE,
            srs_id INTEGER
        );
        CREATE TABLE gpkg_geometry_columns (
            table_name TEXT NOT NULL,
            column_name TEXT NOT NULL,
            geometry_type_name TEXT NOT NULL,
            srs_id INTEGER NOT NULL,
            z TINYINT NOT NULL,
            m TINYINT NOT NULL,
            PRIMARY KEY (table_name, column_name)
        );",
    )?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS gpkg_extensions (
            table_name TEXT,
            column_name TEXT,
            extension_name TEXT NOT NULL,
            definition TEXT NOT NULL,
            scope TEXT NOT NULL,
            PRIMARY KEY (table_name, column_name, extension_name)
        )",
        [],
    )?;
    Ok(())
}

fn insert_standard_spatial_ref_systems(
    tx: &Transaction<'_>,
    custom_srs_id: i32,
    custom_label: &str,
) -> Result<()> {
    let wgs84_wkt = "GEOGCS[\"WGS 84\",DATUM[\"World Geodetic System 1984\",SPHEROID[\"WGS 84\",6378137,298.257223563]],PRIMEM[\"Greenwich\",0],UNIT[\"degree\",0.0174532925199433]]";
    for (srs_name, srs_id, organization, organization_coordsys_id, definition, description) in [
        (
            "Undefined Cartesian",
            -1,
            "NONE",
            -1,
            "undefined",
            "undefined cartesian coordinate reference system",
        ),
        (
            "Undefined Geographic",
            0,
            "NONE",
            0,
            "undefined",
            "undefined geographic coordinate reference system",
        ),
        (
            "WGS 84",
            4326,
            "EPSG",
            4326,
            wgs84_wkt,
            "WGS 84 geodetic coordinate reference system",
        ),
        (
            custom_label,
            custom_srs_id,
            "EPSG",
            custom_srs_id,
            "undefined",
            custom_label,
        ),
    ] {
        tx.execute(
            "INSERT OR IGNORE INTO gpkg_spatial_ref_sys (
                srs_name, srs_id, organization, organization_coordsys_id, definition, description
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                srs_name,
                srs_id,
                organization,
                organization_coordsys_id,
                definition,
                description,
            ],
        )?;
    }
    Ok(())
}

fn create_feature_table(
    tx: &Transaction<'_>,
    table_name: &str,
    cityobjects: &CityObjectTable<'_>,
    srs_id: i32,
    geometry_type: &GeometryType,
    last_change: &str,
) -> Result<()> {
    let geometry_type_name = gpkg_geometry_type_name(geometry_type);
    let mut columns = vec![
        "id INTEGER PRIMARY KEY".to_string(),
        "cityobject_id TEXT NOT NULL".to_string(),
        "cityobject_type TEXT NOT NULL".to_string(),
        "geometry_type TEXT NOT NULL".to_string(),
        "lod TEXT".to_string(),
        format!("{} BLOB NOT NULL", quote_ident(GPKG_GEOM_COLUMN_NAME)),
    ];
    columns.extend(
        cityobjects
            .schema()
            .columns
            .iter()
            .map(|column| format!("{} {}", quote_ident(&column.name), sqlite_type_decl(&column.logical_type))),
    );

    tx.execute_batch(&format!(
        "CREATE TABLE {} ({});",
        quote_ident(table_name),
        columns.join(", ")
    ))?;
    tx.execute(
        "INSERT INTO gpkg_contents (
            table_name, data_type, identifier, description, last_change, srs_id
         ) VALUES (?1, 'features', ?2, ?3, ?4, ?5)",
        params![table_name, table_name, table_name, last_change, srs_id],
    )?;
    tx.execute(
        "INSERT INTO gpkg_geometry_columns (
            table_name, column_name, geometry_type_name, srs_id, z, m
         ) VALUES (?1, ?2, ?3, ?4, 1, 0)",
        params![table_name, GPKG_GEOM_COLUMN_NAME, geometry_type_name, srs_id],
    )?;
    Ok(())
}

fn feature_insert_sql(table_name: &str, schema: &crate::TableSchema<'_>) -> String {
    let mut columns = vec![
        quote_ident("cityobject_id"),
        quote_ident("cityobject_type"),
        quote_ident("geometry_type"),
        quote_ident("lod"),
        quote_ident(GPKG_GEOM_COLUMN_NAME),
    ];
    columns.extend(schema.columns.iter().map(|column| quote_ident(&column.name)));

    let placeholders = (0..columns.len())
        .map(|_| "?".to_string())
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_ident(table_name),
        columns.join(", "),
        placeholders
    )
}

fn insert_feature_row(
    tx: &Transaction<'_>,
    insert_sql: &str,
    row: &CityObjectRow<'_, '_>,
    geometry_type: &GeometryType,
    lod: Option<String>,
    geom_blob: Vec<u8>,
) -> Result<()> {
    let mut params = vec![
        SqlValue::Text(row.cityobject_id.to_string()),
        SqlValue::Text(row.cityobject_type_name().to_string()),
        SqlValue::Text(geometry_type.to_string()),
        lod.map(SqlValue::Text).unwrap_or(SqlValue::Null),
        SqlValue::Blob(geom_blob),
    ];
    params.extend(row.values().map(|value| sqlite_value_from_tabular_value(value?)).collect::<Result<Vec<_>>>()?);
    tx.execute(insert_sql, params_from_iter(params))?;
    Ok(())
}

fn create_relation_table(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE cityobject_relations (
            parent_cityobject_id TEXT NOT NULL,
            child_cityobject_id TEXT NOT NULL,
            PRIMARY KEY (parent_cityobject_id, child_cityobject_id)
        );",
    )?;
    Ok(())
}

fn insert_cityobject_relations(
    tx: &Transaction<'_>,
    relations: &BTreeSet<(String, String)>,
) -> Result<()> {
    let mut statement = tx.prepare(
        "INSERT OR IGNORE INTO cityobject_relations (parent_cityobject_id, child_cityobject_id) VALUES (?1, ?2)",
    )?;
    for (parent, child) in relations {
        statement.execute(params![parent, child])?;
    }
    Ok(())
}

fn create_semantics_table(tx: &Transaction<'_>, schema: &crate::TableSchema<'_>) -> Result<()> {
    let mut columns = vec![
        "semantic_id INTEGER PRIMARY KEY".to_string(),
        "semantic_type TEXT NOT NULL".to_string(),
        "parent INTEGER".to_string(),
        "children TEXT NOT NULL".to_string(),
    ];
    columns.extend(
        schema
            .columns
            .iter()
            .map(|column| format!("{} {}", quote_ident(&column.name), sqlite_type_decl(&column.logical_type))),
    );
    tx.execute_batch(&format!("CREATE TABLE semantics ({});", columns.join(", ")))?;
    Ok(())
}

fn insert_semantics_rows(tx: &Transaction<'_>, semantics: &SemanticTable<'_>) -> Result<()> {
    let insert_sql = semantic_insert_sql(semantics.schema());
    let mut statement = tx.prepare(&insert_sql)?;
    for row in semantics.rows() {
        let fixed = row.fixed();
        let mut params = vec![
            SqlValue::Integer(fixed.semantic_id as i64),
            SqlValue::Text(fixed.semantic_type_name().to_string()),
            fixed.parent.map(|value| SqlValue::Integer(value as i64)).unwrap_or(SqlValue::Null),
            SqlValue::Text(serde_json::to_string(&fixed.children)?),
        ];
        params.extend(
            row.values()
                .map(|value| sqlite_value_from_tabular_value(value?))
                .collect::<Result<Vec<_>>>()?,
        );
        statement.execute(params_from_iter(params))?;
    }
    Ok(())
}

fn semantic_insert_sql(schema: &crate::TableSchema<'_>) -> String {
    let mut columns = vec![
        quote_ident("semantic_id"),
        quote_ident("semantic_type"),
        quote_ident("parent"),
        quote_ident("children"),
    ];
    columns.extend(schema.columns.iter().map(|column| quote_ident(&column.name)));
    let placeholders = (0..columns.len())
        .map(|_| "?".to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO semantics ({}) VALUES ({})",
        columns.join(", "),
        placeholders
    )
}

fn create_semantic_relations_table(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE semantic_relations (
            semantic_id INTEGER,
            cityobject_id TEXT NOT NULL,
            cityobject_ix INTEGER NOT NULL,
            geometry_ix INTEGER NOT NULL,
            geometry_type TEXT NOT NULL,
            geometry_lod TEXT,
            geometry_is_instance INTEGER NOT NULL,
            primitive_type TEXT NOT NULL,
            primitive_ix INTEGER NOT NULL,
            point_ix INTEGER,
            linestring_ix INTEGER,
            solid_ix INTEGER,
            shell_ix INTEGER,
            surface_ix INTEGER
        );",
    )?;
    Ok(())
}

fn insert_semantic_relations(
    tx: &Transaction<'_>,
    assignments: &SemanticAssignmentTable<'_>,
) -> Result<()> {
    let mut statement = tx.prepare(
        "INSERT INTO semantic_relations (
            semantic_id, cityobject_id, cityobject_ix, geometry_ix, geometry_type,
            geometry_lod, geometry_is_instance, primitive_type, primitive_ix,
            point_ix, linestring_ix, solid_ix, shell_ix, surface_ix
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?;
    for row in assignments.rows() {
        statement.execute(params![
            row.semantic_id.map(|value| value as i64),
            row.cityobject_id,
            row.cityobject_ix as i64,
            row.geometry_ix as i64,
            row.geometry_type.to_string(),
            row.geometry_lod.clone(),
            row.geometry_is_instance as i64,
            row.primitive_type.to_string(),
            row.primitive_ix as i64,
            row.point_ix.map(|value| value as i64),
            row.linestring_ix.map(|value| value as i64),
            row.solid_ix.map(|value| value as i64),
            row.shell_ix.map(|value| value as i64),
            row.surface_ix.map(|value| value as i64),
        ])?;
    }
    Ok(())
}

fn create_metadata_tables(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE gpkg_metadata (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            md_scope TEXT NOT NULL,
            md_standard_uri TEXT NOT NULL,
            mime_type TEXT NOT NULL,
            metadata TEXT NOT NULL
        );
        CREATE TABLE gpkg_metadata_reference (
            reference_scope TEXT NOT NULL,
            table_name TEXT,
            column_name TEXT,
            row_id_value INTEGER,
            timestamp TEXT NOT NULL,
            md_file_id INTEGER NOT NULL,
            md_parent_id INTEGER
        );",
    )?;
    tx.execute(
        "INSERT INTO gpkg_extensions (
            table_name, column_name, extension_name, definition, scope
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            Some("gpkg_metadata"),
            Option::<&str>::None,
            "gpkg_metadata",
            GPKG_METADATA_STANDARD_URI,
            "read-write",
        ],
    )?;
    tx.execute(
        "INSERT INTO gpkg_extensions (
            table_name, column_name, extension_name, definition, scope
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            Some("gpkg_metadata_reference"),
            Option::<&str>::None,
            "gpkg_metadata",
            GPKG_METADATA_STANDARD_URI,
            "read-write",
        ],
    )?;
    Ok(())
}

fn insert_metadata_rows(
    tx: &Transaction<'_>,
    metadata_table: &MetadataTable<'_>,
    model: &CityModel,
    last_change: &str,
) -> Result<()> {
    let Some(metadata) = model.metadata() else {
        return Ok(());
    };
    let metadata_json = compact_metadata_json(metadata)?;
    tx.execute(
        "INSERT INTO gpkg_metadata (
            md_scope, md_standard_uri, mime_type, metadata
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            GPKG_METADATA_REFERENCE_SCOPE,
            GPKG_METADATA_STANDARD_URI,
            "application/json",
            metadata_json,
        ],
    )?;
    let metadata_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO gpkg_metadata_reference (
            reference_scope, table_name, column_name, row_id_value, timestamp, md_file_id, md_parent_id
         ) VALUES (?1, NULL, NULL, NULL, ?2, ?3, NULL)",
        params![GPKG_METADATA_REFERENCE_SCOPE, last_change, metadata_id],
    )?;

    let _ = metadata_table;
    Ok(())
}

fn compact_metadata_json(metadata: &Metadata<OwnedStringStorage>) -> Result<String> {
    let mut object = serde_json::Map::new();
    if let Some(identifier) = metadata.identifier() {
        object.insert(
            "identifier".to_string(),
            serde_json::Value::String(identifier.to_string()),
        );
    }
    if let Some(reference_date) = metadata.reference_date() {
        object.insert(
            "referenceDate".to_string(),
            serde_json::Value::String(reference_date.to_string()),
        );
    }
    if let Some(reference_system) = metadata.reference_system() {
        object.insert(
            "referenceSystem".to_string(),
            serde_json::Value::String(reference_system.to_string()),
        );
    }
    if let Some(title) = metadata.title() {
        object.insert("title".to_string(), serde_json::Value::String(title.to_string()));
    }
    if let Some(extent) = metadata.geographical_extent() {
        object.insert(
            "geographicalExtent".to_string(),
            serde_json::Value::Array(extent.as_slice().iter().copied().map(serde_json::Value::from).collect()),
        );
    }
    if let Some(contact) = metadata.point_of_contact() {
        let mut contact_object = serde_json::Map::new();
        contact_object.insert(
            "contactName".to_string(),
            serde_json::Value::String(contact.contact_name().to_string()),
        );
        contact_object.insert(
            "emailAddress".to_string(),
            serde_json::Value::String(contact.email_address().to_string()),
        );
        if let Some(role) = contact.role() {
            contact_object.insert("role".to_string(), serde_json::Value::String(role.to_string()));
        }
        if let Some(website) = contact.website().as_ref() {
            contact_object.insert(
                "website".to_string(),
                serde_json::Value::String(website.to_string()),
            );
        }
        if let Some(kind) = contact.contact_type() {
            contact_object.insert("contactType".to_string(), serde_json::Value::String(kind.to_string()));
        }
        if let Some(phone) = contact.phone().as_ref() {
            contact_object.insert(
                "phone".to_string(),
                serde_json::Value::String(phone.to_string()),
            );
        }
        if let Some(organization) = contact.organization().as_ref() {
            contact_object.insert(
                "organization".to_string(),
                serde_json::Value::String(organization.to_string()),
            );
        }
        object.insert("pointOfContact".to_string(), serde_json::Value::Object(contact_object));
    }
    if let Some(extra) = metadata.extra() {
        for (name, value) in extra.iter() {
            object.insert(format!("+{}", name), attribute_value_to_json(value)?);
        }
    }
    Ok(serde_json::to_string(&serde_json::Value::Object(object))?)
}

fn update_feature_layer_extents(
    tx: &Transaction<'_>,
    feature_layers: &BTreeMap<String, FeatureLayerState>,
) -> Result<()> {
    for (table_name, layer) in feature_layers {
        if let Some([min_x, min_y, _, max_x, max_y, _]) = layer.extent {
            tx.execute(
                "UPDATE gpkg_contents SET min_x = ?1, min_y = ?2, max_x = ?3, max_y = ?4 WHERE table_name = ?5",
                params![min_x, min_y, max_x, max_y, table_name],
            )?;
        }
    }
    Ok(())
}

fn collect_cityobject_relations(
    row: &CityObjectRow<'_, '_>,
    relations: &mut BTreeSet<(String, String)>,
) -> Result<()> {
    let cityobject_id = row.cityobject_id.to_string();
    for parent in row.parents()?.iter() {
        relations.insert((parent.to_string(), cityobject_id.clone()));
    }
    for child in row.children()?.iter() {
        relations.insert((cityobject_id.clone(), child.to_string()));
    }
    Ok(())
}

fn encode_geometry_blob(
    model: &CityModel,
    geometry_type: &GeometryType,
    boundary: &Boundary<u32>,
    srs_id: i32,
) -> Result<EncodedGeometry> {
    let mut payload = Vec::new();
    let mut envelope = None;
    let empty = match *geometry_type {
        GeometryType::MultiPoint => {
            let points = boundary.to_nested_multi_point()?;
            write_wkb_multipoint(&mut payload, model, &points, &mut envelope)?;
            points.is_empty()
        }
        GeometryType::MultiLineString => {
            let linestrings = boundary.to_nested_multi_linestring()?;
            write_wkb_multilinestring(&mut payload, model, &linestrings, &mut envelope)?;
            linestrings.is_empty()
        }
        GeometryType::MultiSurface | GeometryType::CompositeSurface => {
            let polygons = boundary.to_nested_multi_or_composite_surface()?;
            write_wkb_multipolygon(&mut payload, model, &polygons, &mut envelope)?;
            polygons.is_empty()
        }
        GeometryType::Solid => {
            let polygons = boundary
                .to_nested_solid()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            write_wkb_multipolygon(&mut payload, model, &polygons, &mut envelope)?;
            polygons.is_empty()
        }
        GeometryType::MultiSolid | GeometryType::CompositeSolid => {
            let polygons = boundary
                .to_nested_multi_or_composite_solid()?
                .into_iter()
                .flatten()
                .flatten()
                .collect::<Vec<_>>();
            write_wkb_multipolygon(&mut payload, model, &polygons, &mut envelope)?;
            polygons.is_empty()
        }
        GeometryType::GeometryInstance => {
            bail!("GeometryInstance should have been resolved before GeoPackage export")
        }
        _ => {
            bail!("unsupported geometry type {geometry_type}")
        }
    };

    Ok(EncodedGeometry {
        blob: wrap_geopackage_binary(payload, envelope, empty, srs_id),
        envelope,
    })
}

#[derive(Debug)]
struct EncodedGeometry {
    blob: Vec<u8>,
    envelope: Option<[f64; 6]>,
}

fn write_wkb_multipoint(
    writer: &mut Vec<u8>,
    model: &CityModel,
    points: &[u32],
    envelope: &mut Option<[f64; 6]>,
) -> Result<()> {
    write_u8(writer, 1)?;
    write_u32(writer, 1004)?;
    write_u32(writer, points.len() as u32)?;
    for point in points {
        write_wkb_point(writer, model, *point, envelope)?;
    }
    Ok(())
}

fn write_wkb_multilinestring(
    writer: &mut Vec<u8>,
    model: &CityModel,
    linestrings: &[Vec<u32>],
    envelope: &mut Option<[f64; 6]>,
) -> Result<()> {
    write_u8(writer, 1)?;
    write_u32(writer, 1005)?;
    write_u32(writer, linestrings.len() as u32)?;
    for linestring in linestrings {
        write_wkb_linestring(writer, model, linestring, envelope)?;
    }
    Ok(())
}

fn write_wkb_multipolygon(
    writer: &mut Vec<u8>,
    model: &CityModel,
    polygons: &[Vec<Vec<u32>>],
    envelope: &mut Option<[f64; 6]>,
) -> Result<()> {
    write_u8(writer, 1)?;
    write_u32(writer, 1006)?;
    write_u32(writer, polygons.len() as u32)?;
    for polygon in polygons {
        write_wkb_polygon(writer, model, polygon, envelope)?;
    }
    Ok(())
}

fn write_wkb_point(
    writer: &mut Vec<u8>,
    model: &CityModel,
    vertex_index: u32,
    envelope: &mut Option<[f64; 6]>,
) -> Result<()> {
    let coordinate = vertex_coordinates(model, vertex_index)?;
    write_u8(writer, 1)?;
    write_u32(writer, 1001)?;
    write_coordinate(writer, coordinate, envelope)?;
    Ok(())
}

fn write_wkb_linestring(
    writer: &mut Vec<u8>,
    model: &CityModel,
    line: &[u32],
    envelope: &mut Option<[f64; 6]>,
) -> Result<()> {
    write_u8(writer, 1)?;
    write_u32(writer, 1002)?;
    write_u32(writer, line.len() as u32)?;
    for vertex_index in line {
        let coordinate = vertex_coordinates(model, *vertex_index)?;
        write_coordinate(writer, coordinate, envelope)?;
    }
    Ok(())
}

fn write_wkb_polygon(
    writer: &mut Vec<u8>,
    model: &CityModel,
    polygon: &[Vec<u32>],
    envelope: &mut Option<[f64; 6]>,
) -> Result<()> {
    write_u8(writer, 1)?;
    write_u32(writer, 1003)?;
    write_u32(writer, polygon.len() as u32)?;
    for ring in polygon {
        write_u32(writer, ring.len() as u32)?;
        for vertex_index in ring {
            let coordinate = vertex_coordinates(model, *vertex_index)?;
            write_coordinate(writer, coordinate, envelope)?;
        }
    }
    Ok(())
}

fn vertex_coordinates(model: &CityModel, vertex_index: u32) -> Result<[f64; 3]> {
    let vertex = model
        .get_vertex(VertexIndex::new(vertex_index))
        .with_context(|| format!("missing vertex {vertex_index}"))?;
    Ok(vertex.to_array())
}

fn write_coordinate(
    writer: &mut Vec<u8>,
    coordinate: [f64; 3],
    envelope: &mut Option<[f64; 6]>,
) -> Result<()> {
    update_envelope(envelope, coordinate);
    writer.write_all(&coordinate[0].to_le_bytes())?;
    writer.write_all(&coordinate[1].to_le_bytes())?;
    writer.write_all(&coordinate[2].to_le_bytes())?;
    Ok(())
}

fn update_envelope(envelope: &mut Option<[f64; 6]>, coordinate: [f64; 3]) {
    match envelope {
        Some(existing) => {
            existing[0] = existing[0].min(coordinate[0]);
            existing[1] = existing[1].min(coordinate[1]);
            existing[2] = existing[2].min(coordinate[2]);
            existing[3] = existing[3].max(coordinate[0]);
            existing[4] = existing[4].max(coordinate[1]);
            existing[5] = existing[5].max(coordinate[2]);
        }
        None => {
            *envelope = Some([
                coordinate[0],
                coordinate[1],
                coordinate[2],
                coordinate[0],
                coordinate[1],
                coordinate[2],
            ]);
        }
    }
}

fn wrap_geopackage_binary(payload: Vec<u8>, envelope: Option<[f64; 6]>, empty: bool, srs_id: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + envelope.map(|_| 48).unwrap_or(0) + payload.len());
    out.extend_from_slice(GPB_MAGIC);
    out.push(0);

    let mut flags = 0b0000_0001;
    if empty {
        flags |= 0b0001_0000;
    }
    if envelope.is_some() {
        flags |= 0b0000_0100;
    }
    out.push(flags);
    out.extend_from_slice(&srs_id.to_le_bytes());
    if let Some(envelope) = envelope {
        for value in envelope {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&payload);
    out
}

fn gpkg_geometry_type_name(geometry_type: &GeometryType) -> &'static str {
    match *geometry_type {
        GeometryType::MultiPoint => "MULTIPOINT",
        GeometryType::MultiLineString => "MULTILINESTRING",
        GeometryType::MultiSurface
        | GeometryType::CompositeSurface
        | GeometryType::Solid
        | GeometryType::MultiSolid
        | GeometryType::CompositeSolid => "MULTIPOLYGON",
        GeometryType::GeometryInstance => "GEOMETRYCOLLECTION",
        _ => "MULTIPOLYGON",
    }
}

fn layer_table_name(
    cityobject_type: String,
    geometry_family: String,
    lod: Option<String>,
    split_lod: bool,
    used_names: &mut HashSet<String>,
) -> String {
    let mut base = format!(
        "{}_{}",
        sanitize_identifier(&cityobject_type),
        sanitize_identifier(&geometry_family)
    );
    if split_lod {
        let lod = lod
            .as_deref()
            .map(sanitize_lod_fragment)
            .unwrap_or_else(|| "none".to_string());
        base.push_str("_lod");
        base.push_str(&lod);
    }
    unique_identifier(base, used_names)
}

fn sanitize_lod_fragment(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_was_underscore = false;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            char::from(95)
        };
        if mapped == char::from(95) {
            if previous_was_underscore {
                continue;
            }
            previous_was_underscore = true;
        } else {
            previous_was_underscore = false;
        }
        sanitized.push(mapped);
    }
    sanitized.trim_matches(char::from(95)).to_string()
}

fn unique_identifier(base: String, used_names: &mut HashSet<String>) -> String {
    if used_names.insert(base.clone()) {
        return base;
    }

    let mut index = 2usize;
    loop {
        let candidate = format!("{}_{}", base, index);
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}

fn sanitize_identifier(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_was_underscore = false;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if previous_was_underscore {
                continue;
            }
            previous_was_underscore = true;
        } else {
            previous_was_underscore = false;
        }
        sanitized.push(mapped);
    }
    let trimmed = sanitized.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "layer".to_string()
    } else if trimmed.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("layer_{trimmed}")
    } else {
        trimmed
    }
}

fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sqlite_type_decl(logical_type: &LogicalType<'_>) -> &'static str {
    match logical_type {
        LogicalType::Boolean | LogicalType::UInt64 | LogicalType::Int64 | LogicalType::GeometryRef => {
            "INTEGER"
        }
        LogicalType::Float64 => "REAL",
        LogicalType::Utf8 => "TEXT",
        LogicalType::Json | LogicalType::Null | LogicalType::List { .. } | LogicalType::Struct(_) => {
            "TEXT"
        }
    }
}

fn sqlite_value_from_tabular_value(value: Value<'_, '_>) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Boolean(value) => SqlValue::Integer(if value { 1 } else { 0 }),
        Value::UInt64(value) => match i64::try_from(value) {
            Ok(value) => SqlValue::Integer(value),
            Err(_) => SqlValue::Text(value.to_string()),
        },
        Value::Int64(value) => SqlValue::Integer(value),
        Value::Float64(value) => SqlValue::Real(value),
        Value::Utf8(value) => SqlValue::Text(value.to_string()),
        Value::GeometryRef(value) => SqlValue::Integer(i64::from(value.raw_parts().0)),
        Value::List(values) => {
            let json = value_to_json(Value::List(values))?;
            SqlValue::Text(serde_json::to_string(&json)?)
        }
        Value::Struct(values) => {
            let json = value_to_json(Value::Struct(values))?;
            SqlValue::Text(serde_json::to_string(&json)?)
        }
        Value::Json(value) => SqlValue::Text(serde_json::to_string(&attribute_value_to_json(value)?)?),
    })
}

fn value_to_json(value: Value<'_, '_>) -> Result<serde_json::Value> {
    Ok(match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => serde_json::Value::Bool(value),
        Value::UInt64(value) => serde_json::Value::Number(value.into()),
        Value::Int64(value) => serde_json::Value::Number(value.into()),
        Value::Float64(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Utf8(value) => serde_json::Value::String(value.to_string()),
        Value::GeometryRef(value) => serde_json::Value::Number((value.raw_parts().0 as i64).into()),
        Value::List(values) => {
            let mut items = Vec::with_capacity(values.len());
            for item in values.iter() {
                items.push(value_to_json(item?)?);
            }
            serde_json::Value::Array(items)
        }
        Value::Struct(values) => {
            let mut fields = Map::new();
            for field in values.fields() {
                let (name, value) = field?;
                fields.insert(name.to_string(), value_to_json(value)?);
            }
            serde_json::Value::Object(fields)
        }
        Value::Json(value) => attribute_value_to_json(value)?,
    })
}

fn attribute_value_to_json(value: &OwnedAttributeValue) -> Result<serde_json::Value> {
    Ok(match value {
        OwnedAttributeValue::Null => serde_json::Value::Null,
        OwnedAttributeValue::Bool(value) => serde_json::Value::Bool(*value),
        OwnedAttributeValue::Unsigned(value) => serde_json::Value::Number((*value).into()),
        OwnedAttributeValue::Integer(value) => serde_json::Value::Number((*value).into()),
        OwnedAttributeValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        OwnedAttributeValue::String(value) => serde_json::Value::String(value.clone()),
        OwnedAttributeValue::Vec(values) => serde_json::Value::Array(
            values
                .iter()
                .map(attribute_value_to_json)
                .collect::<Result<Vec<_>>>()?,
        ),
        OwnedAttributeValue::Map(values) => {
            let mut fields = Map::new();
            for (name, value) in values {
                fields.insert(name.clone(), attribute_value_to_json(value)?);
            }
            serde_json::Value::Object(fields)
        }
        OwnedAttributeValue::Geometry(value) => {
            serde_json::Value::Number((value.raw_parts().0 as i64).into())
        }
        unsupported => bail!("unsupported attribute value variant {unsupported}"),
    })
}

fn union_bbox(existing: Option<[f64; 6]>, new_bbox: Option<[f64; 6]>) -> Option<[f64; 6]> {
    let new_bbox = new_bbox?;
    match existing {
        Some(mut current) => {
            current[0] = current[0].min(new_bbox[0]);
            current[1] = current[1].min(new_bbox[1]);
            current[2] = current[2].min(new_bbox[2]);
            current[3] = current[3].max(new_bbox[3]);
            current[4] = current[4].max(new_bbox[4]);
            current[5] = current[5].max(new_bbox[5]);
            Some(current)
        }
        None => Some(new_bbox),
    }
}

fn write_u8(writer: &mut Vec<u8>, value: u8) -> Result<()> {
    writer.write_all(&[value])?;
    Ok(())
}

fn write_u32(writer: &mut Vec<u8>, value: u32) -> Result<()> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_epsg_srs_id, sanitize_identifier, wrap_geopackage_binary};

    #[test]
    fn parses_epsg_codes_from_common_crs_strings() {
        assert_eq!(parse_epsg_srs_id("EPSG:7415"), Some(7415));
        assert_eq!(
            parse_epsg_srs_id("https://www.opengis.net/def/crs/EPSG/0/7415"),
            Some(7415)
        );
        assert_eq!(parse_epsg_srs_id("OGC:CRS84"), None);
    }

    #[test]
    fn sanitizes_layer_identifiers() {
        assert_eq!(sanitize_identifier("Building Part"), "building_part");
        assert_eq!(sanitize_identifier("+NoiseBuilding"), "noisebuilding");
        assert_eq!(sanitize_identifier("123abc"), "layer_123abc");
    }

    #[test]
    fn wraps_geopackage_binary_with_header_and_optional_envelope() {
        let blob = wrap_geopackage_binary(vec![1, 2, 3], Some([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), false, 7415);
        assert_eq!(&blob[0..2], b"GP");
        assert_eq!(blob[2], 0);
        assert_eq!(blob[3] & 0b0000_0001, 1);
        assert_eq!(i32::from_le_bytes(blob[4..8].try_into().unwrap()), 7415);
        assert_eq!(blob[3] & 0b0000_0100, 0b0000_0100);
        assert_eq!(blob.len(), 2 + 1 + 1 + 4 + 48 + 3);
    }
}
