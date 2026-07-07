use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use cityjson_lib::cityjson_types::v2_0::boundary::Boundary;
use cityjson_lib::cityjson_types::v2_0::{GeometryType, VertexIndex};
use cityjson_lib::CityModel;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, Transaction};

use crate::{
    tabular::{
        geometry_ref_to_wkb, metadata_to_compact_json, semantic_primitive_geometry, value_to_json,
    },
    tabulate_addresses, tabulate_cityobject_hierarchy, tabulate_cityobjects,
    tabulate_model_metadata, tabulate_semantic_hierarchy, tabulate_semantic_primitives,
    AddressTable, CityObjectHierarchyTable, CityObjectRow, CityObjectTable, LogicalType,
    MetadataTable, SemanticHierarchyTable, SemanticPrimitiveTable, Value,
};

const GPKG_APPLICATION_ID: i32 = 0x4750_4b47;
const GPKG_USER_VERSION: i32 = 10300;
const GPB_MAGIC: &[u8; 2] = b"GP";
const GPKG_GEOM_COLUMN_NAME: &str = "geom";
const GPKG_METADATA_STANDARD_URI: &str = "https://www.geopackage.org/spec/#extension_metadata";
const GPKG_CRS_WKT_EXTENSION_URI: &str = "https://www.geopackage.org/spec/#extension_crs_wkt";
const GPKG_METADATA_REFERENCE_SCOPE: &str = "dataset";
const SQLITE_LAST_CHANGE_SQL: &str = "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct GpkgExportOptions {
    pub split_lod: bool,
    pub include_semantics: bool,
    pub include_address: bool,
    pub include_hierarchy: bool,
    pub include_metadata: bool,
    pub output_crs: Option<String>,
}

#[derive(Debug)]
struct FeatureLayerState {
    insert_sql: String,
    extent: Option<[f64; 6]>,
}

type FeatureLayerKey = (String, String, Option<String>);

/// Converts a `CityJSON` model to a `GeoPackage` file.
///
/// # Errors
///
/// Returns an error when the output directory cannot be created, metadata or
/// geometry cannot be resolved, or the `GeoPackage` cannot be written.
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
    let hierarchy = if options.include_hierarchy {
        Some((
            tabulate_cityobject_hierarchy(model)?,
            tabulate_semantic_hierarchy(model),
        ))
    } else {
        None
    };
    let semantics = if options.include_semantics {
        Some(tabulate_semantic_primitives(model)?)
    } else {
        None
    };
    let addresses = if options.include_address {
        Some(tabulate_addresses(model)?)
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
    insert_standard_spatial_ref_systems(&tx, &resolved_srs)?;

    let mut used_table_names = HashSet::new();
    let mut feature_layer_names = BTreeMap::new();
    let mut feature_layers: BTreeMap<String, FeatureLayerState> = BTreeMap::new();

    export_feature_layers(
        &FeatureLayerExport {
            tx: &tx,
            model,
            cityobjects: &cityobjects,
            cityobject_rows: &cityobject_rows,
            srs_id: resolved_srs.srs_id,
            split_lod: options.split_lod,
            last_change: &last_change,
        },
        &mut used_table_names,
        &mut feature_layer_names,
        &mut feature_layers,
    )?;

    if let Some((cityobject_hierarchy, semantic_hierarchy)) = hierarchy {
        create_cityobject_hierarchy_table(&tx, &last_change)?;
        insert_cityobject_hierarchy(&tx, &cityobject_hierarchy)?;
        create_semantic_hierarchy_table(&tx, &last_change)?;
        insert_semantic_hierarchy(&tx, &semantic_hierarchy)?;
    }

    if let Some(semantics_table) = semantics {
        create_semantics_table(&tx, &semantics_table, resolved_srs.srs_id, &last_change)?;
        let extent = insert_semantics_rows(&tx, &semantics_table, resolved_srs.srs_id)?;
        feature_layers.insert(
            "semantics".to_string(),
            FeatureLayerState {
                insert_sql: String::new(),
                extent,
            },
        );
    }

    if let Some(address_table) = addresses {
        create_address_table(&tx, &address_table, resolved_srs.srs_id, &last_change)?;
        let extent = insert_address_rows(&tx, &address_table, resolved_srs.srs_id)?;
        feature_layers.insert(
            "addresses".to_string(),
            FeatureLayerState {
                insert_sql: String::new(),
                extent,
            },
        );
    }

    if let Some(metadata_table) = metadata {
        create_metadata_tables(&tx)?;
        insert_metadata_rows(&tx, &metadata_table, model, &last_change)?;
    }

    update_feature_layer_extents(&tx, &feature_layers)?;

    tx.commit()?;
    Ok(())
}

struct FeatureLayerExport<'tx, 'conn, 'model, 'rows> {
    tx: &'tx Transaction<'conn>,
    model: &'model CityModel,
    cityobjects: &'rows CityObjectTable<'model>,
    cityobject_rows: &'rows [CityObjectRow<'rows, 'model>],
    srs_id: i32,
    split_lod: bool,
    last_change: &'tx str,
}

fn export_feature_layers(
    export: &FeatureLayerExport<'_, '_, '_, '_>,
    used_table_names: &mut HashSet<String>,
    feature_layer_names: &mut BTreeMap<FeatureLayerKey, String>,
    feature_layers: &mut BTreeMap<String, FeatureLayerState>,
) -> Result<()> {
    for (cityobject_ix, (_, cityobject)) in export.model.cityobjects().iter().enumerate() {
        let row = &export.cityobject_rows[cityobject_ix];

        let Some(geometry_handles) = cityobject.geometry() else {
            continue;
        };

        for geometry_handle in geometry_handles.iter().copied() {
            let geometry = export.model.resolve_geometry(geometry_handle)?;
            let Some(boundary) = geometry.boundaries() else {
                continue;
            };

            let geometry_type = *geometry.type_geometry();
            let encoded =
                encode_geometry_blob(export.model, geometry_type, boundary, export.srs_id)?;
            let cityobject_type = row.cityobject_type_name().to_string();
            let geometry_family = geometry_type.to_string();
            let lod = geometry.lod().map(ToString::to_string);
            let layer_key = (
                cityobject_type.clone(),
                geometry_family.clone(),
                lod.clone(),
            );
            let layer_table_name = feature_layer_names
                .entry(layer_key)
                .or_insert_with(|| {
                    let base = layer_table_name_base(
                        &cityobject_type,
                        &geometry_family,
                        lod.as_deref(),
                        export.split_lod,
                    );
                    unique_identifier(base, used_table_names)
                })
                .clone();
            let state = match feature_layers.entry(layer_table_name) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let table_name = entry.key().clone();
                    create_feature_table(
                        export.tx,
                        &table_name,
                        export.cityobjects,
                        export.srs_id,
                        geometry_type,
                        export.last_change,
                    )?;
                    entry.insert(FeatureLayerState {
                        insert_sql: feature_insert_sql(&table_name, export.cityobjects.schema()),
                        extent: None,
                    })
                }
            };
            state.extent = union_bbox(state.extent, encoded.envelope);
            insert_feature_row(
                export.tx,
                &state.insert_sql,
                export.model,
                row,
                geometry_type,
                geometry.lod().map(ToString::to_string),
                encoded.blob,
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct ResolvedSrs {
    srs_id: i32,
    label: String,
    definition: String,
}

fn resolve_srs(model: &CityModel, options: &GpkgExportOptions) -> Result<ResolvedSrs> {
    if let Some(metadata) = model.metadata() {
        if let Some(reference_system) = metadata.reference_system() {
            let reference_system = reference_system.to_string();
            if let Some(srs_id) = parse_epsg_srs_id(&reference_system) {
                return resolved_epsg_srs(srs_id, reference_system);
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

    resolved_epsg_srs(srs_id, output_crs.clone())
}

fn resolved_epsg_srs(srs_id: i32, label: String) -> Result<ResolvedSrs> {
    let definition = epsg_wkt_definition(srs_id)
        .with_context(|| format!("resolve EPSG:{srs_id} WKT definition for GeoPackage SRS"))?;
    Ok(ResolvedSrs {
        srs_id,
        label,
        definition,
    })
}

#[cfg(any(feature = "proj-system", feature = "proj-bundled"))]
fn epsg_wkt_definition(srs_id: i32) -> Result<String> {
    use std::ffi::{CStr, CString};

    use proj_sys::{
        proj_as_wkt, proj_context_create, proj_context_destroy, proj_create_from_database,
        proj_destroy, PJ_CATEGORY_PJ_CATEGORY_CRS, PJ_WKT_TYPE_PJ_WKT2_2019,
    };

    let auth_name = CString::new("EPSG")?;
    let code = CString::new(srs_id.to_string())?;
    let ctx = unsafe { proj_context_create() };
    if ctx.is_null() {
        bail!("PROJ could not create a context");
    }

    let object = unsafe {
        proj_create_from_database(
            ctx,
            auth_name.as_ptr(),
            code.as_ptr(),
            PJ_CATEGORY_PJ_CATEGORY_CRS,
            0,
            std::ptr::null(),
        )
    };
    if object.is_null() {
        unsafe { proj_context_destroy(ctx) };
        bail!("EPSG:{srs_id} was not found in the PROJ database");
    }

    let wkt = unsafe { proj_as_wkt(ctx, object, PJ_WKT_TYPE_PJ_WKT2_2019, std::ptr::null()) };
    let result = if wkt.is_null() {
        Err(anyhow::anyhow!(
            "PROJ could not export EPSG:{srs_id} as WKT2"
        ))
    } else {
        Ok(unsafe { CStr::from_ptr(wkt) }.to_str()?.to_string())
    };

    unsafe {
        proj_destroy(object);
        proj_context_destroy(ctx);
    }
    result
}

#[cfg(not(any(feature = "proj-system", feature = "proj-bundled")))]
fn epsg_wkt_definition(srs_id: i32) -> Result<String> {
    if srs_id == 4326 {
        Ok("GEOGCS[\"WGS 84\",DATUM[\"World Geodetic System 1984\",SPHEROID[\"WGS 84\",6378137,298.257223563]],PRIMEM[\"Greenwich\",0],UNIT[\"degree\",0.0174532925199433]]".to_string())
    } else {
        bail!("GeoPackage SRS WKT export for EPSG:{srs_id} requires the proj-system or proj-bundled feature")
    }
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
            description TEXT NOT NULL,
            definition_12_063 TEXT NOT NULL
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
    tx.execute(
        "INSERT INTO gpkg_extensions (
            table_name, column_name, extension_name, definition, scope
         ) VALUES ('gpkg_spatial_ref_sys', 'definition_12_063', 'gpkg_crs_wkt', ?1, 'read-write')",
        params![GPKG_CRS_WKT_EXTENSION_URI],
    )?;
    Ok(())
}

fn insert_standard_spatial_ref_systems(tx: &Transaction<'_>, custom: &ResolvedSrs) -> Result<()> {
    let wgs84_definition = epsg_wkt_definition(4326).context("resolve EPSG:4326 WKT definition")?;
    for (srs_name, srs_id, organization, organization_coordsys_id, definition, description) in [
        (
            "Undefined Cartesian",
            -1,
            "NONE",
            -1,
            "undefined".to_string(),
            "undefined cartesian coordinate reference system",
        ),
        (
            "Undefined Geographic",
            0,
            "NONE",
            0,
            "undefined".to_string(),
            "undefined geographic coordinate reference system",
        ),
        (
            "WGS 84",
            4326,
            "EPSG",
            4326,
            wgs84_definition,
            "WGS 84 geodetic coordinate reference system",
        ),
        (
            custom.label.as_str(),
            custom.srs_id,
            "EPSG",
            custom.srs_id,
            custom.definition.clone(),
            custom.label.as_str(),
        ),
    ] {
        tx.execute(
            "INSERT OR IGNORE INTO gpkg_spatial_ref_sys (
                srs_name, srs_id, organization, organization_coordsys_id,
                definition, description, definition_12_063
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                srs_name,
                srs_id,
                organization,
                organization_coordsys_id,
                definition,
                description,
                definition,
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
    geometry_type: GeometryType,
    last_change: &str,
) -> Result<()> {
    let geometry_type_name = gpkg_geometry_type_name(geometry_type);
    let mut columns = vec![
        "id INTEGER PRIMARY KEY".to_string(),
        "cityobject_id TEXT NOT NULL".to_string(),
        "cityobject_type TEXT NOT NULL".to_string(),
        "geometry_type TEXT NOT NULL".to_string(),
        "lod TEXT".to_string(),
        format!(
            "{} {} NOT NULL",
            quote_ident(GPKG_GEOM_COLUMN_NAME),
            geometry_type_name
        ),
    ];
    columns.extend(cityobjects.schema().columns.iter().map(|column| {
        format!(
            "{} {}",
            quote_ident(&column.name),
            sqlite_type_decl(&column.logical_type)
        )
    }));

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
        params![
            table_name,
            GPKG_GEOM_COLUMN_NAME,
            geometry_type_name,
            srs_id
        ],
    )?;
    Ok(())
}

fn create_address_table(
    tx: &Transaction<'_>,
    addresses: &AddressTable<'_>,
    srs_id: i32,
    last_change: &str,
) -> Result<()> {
    let mut columns = vec![
        "id INTEGER PRIMARY KEY".to_string(),
        "cityobject_id TEXT NOT NULL".to_string(),
        "cityobject_type TEXT NOT NULL".to_string(),
    ];
    columns.extend(addresses.schema().columns.iter().map(|column| {
        format!(
            "{} {}",
            quote_ident(&column.name),
            sqlite_type_decl(&column.logical_type)
        )
    }));
    columns.push(format!("{} MULTIPOINT", quote_ident(GPKG_GEOM_COLUMN_NAME)));

    tx.execute_batch(&format!("CREATE TABLE addresses ({});", columns.join(", ")))?;
    tx.execute(
        "INSERT INTO gpkg_contents (
            table_name, data_type, identifier, description, last_change, srs_id
         ) VALUES ('addresses', 'features', 'addresses', 'addresses', ?1, ?2)",
        params![last_change, srs_id],
    )?;
    tx.execute(
        "INSERT INTO gpkg_geometry_columns (
            table_name, column_name, geometry_type_name, srs_id, z, m
         ) VALUES ('addresses', ?1, 'MULTIPOINT', ?2, 1, 0)",
        params![GPKG_GEOM_COLUMN_NAME, srs_id],
    )?;
    Ok(())
}

fn address_insert_sql(schema: &crate::TableSchema<'_>) -> String {
    let mut columns = vec![quote_ident("cityobject_id"), quote_ident("cityobject_type")];
    columns.extend(
        schema
            .columns
            .iter()
            .map(|column| quote_ident(&column.name)),
    );
    columns.push(quote_ident(GPKG_GEOM_COLUMN_NAME));
    let placeholders = (0..columns.len())
        .map(|_| "?".to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO addresses ({}) VALUES ({})",
        columns.join(", "),
        placeholders
    )
}

fn insert_address_rows(
    tx: &Transaction<'_>,
    addresses: &AddressTable<'_>,
    srs_id: i32,
) -> Result<Option<[f64; 6]>> {
    let insert_sql = address_insert_sql(addresses.schema());
    let mut statement = tx.prepare(&insert_sql)?;
    let mut extent = None;
    for row in addresses.rows() {
        let fixed = row.fixed();
        let encoded = fixed
            .location()?
            .map(|handle| encode_address_geometry_blob(addresses.model(), handle, srs_id))
            .transpose()?;
        extent = union_bbox(
            extent,
            encoded.as_ref().and_then(|encoded| encoded.envelope),
        );
        let mut params = vec![
            SqlValue::Text(fixed.cityobject_id.to_string()),
            SqlValue::Text(fixed.cityobject_type_name().to_string()),
        ];
        params.extend(
            row.values()
                .map(|value| sqlite_value_from_tabular_value(addresses.model(), value?))
                .collect::<Result<Vec<_>>>()?,
        );
        params.push(encoded.map_or(SqlValue::Null, |encoded| SqlValue::Blob(encoded.blob)));
        statement.execute(params_from_iter(params))?;
    }
    Ok(extent)
}

fn feature_insert_sql(table_name: &str, schema: &crate::TableSchema<'_>) -> String {
    let mut columns = vec![
        quote_ident("cityobject_id"),
        quote_ident("cityobject_type"),
        quote_ident("geometry_type"),
        quote_ident("lod"),
        quote_ident(GPKG_GEOM_COLUMN_NAME),
    ];
    columns.extend(
        schema
            .columns
            .iter()
            .map(|column| quote_ident(&column.name)),
    );

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
    model: &CityModel,
    row: &CityObjectRow<'_, '_>,
    geometry_type: GeometryType,
    lod: Option<String>,
    geom_blob: Vec<u8>,
) -> Result<()> {
    let mut params = vec![
        SqlValue::Text(row.cityobject_id.to_string()),
        SqlValue::Text(row.cityobject_type_name().to_string()),
        SqlValue::Text(geometry_type.to_string()),
        lod.map_or(SqlValue::Null, SqlValue::Text),
        SqlValue::Blob(geom_blob),
    ];
    params.extend(
        row.values()
            .map(|value| sqlite_value_from_tabular_value(model, value?))
            .collect::<Result<Vec<_>>>()?,
    );
    let mut statement = tx.prepare_cached(insert_sql)?;
    statement.execute(params_from_iter(params))?;
    Ok(())
}

fn register_attribute_table(
    tx: &Transaction<'_>,
    table_name: &str,
    description: &str,
    last_change: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO gpkg_contents (
            table_name, data_type, identifier, description, last_change
         ) VALUES (?1, 'attributes', ?2, ?3, ?4)",
        params![table_name, table_name, description, last_change],
    )?;
    Ok(())
}

fn create_cityobject_hierarchy_table(tx: &Transaction<'_>, last_change: &str) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE cityobject_hierarchy (
            parent_id TEXT NOT NULL,
            child_id TEXT NOT NULL,
            PRIMARY KEY (parent_id, child_id)
        );",
    )?;
    register_attribute_table(
        tx,
        "cityobject_hierarchy",
        "cityobject_hierarchy",
        last_change,
    )
}

fn insert_cityobject_hierarchy(
    tx: &Transaction<'_>,
    hierarchy: &CityObjectHierarchyTable<'_>,
) -> Result<()> {
    let mut statement = tx.prepare(
        "INSERT OR IGNORE INTO cityobject_hierarchy (parent_id, child_id) VALUES (?1, ?2)",
    )?;
    for row in hierarchy.rows() {
        statement.execute(params![row.parent_id, row.child_id])?;
    }
    Ok(())
}

fn create_semantic_hierarchy_table(tx: &Transaction<'_>, last_change: &str) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE semantic_hierarchy (
            parent_id INTEGER NOT NULL,
            child_id INTEGER NOT NULL,
            PRIMARY KEY (parent_id, child_id)
        );",
    )?;
    register_attribute_table(tx, "semantic_hierarchy", "semantic_hierarchy", last_change)
}

fn insert_semantic_hierarchy(
    tx: &Transaction<'_>,
    hierarchy: &SemanticHierarchyTable,
) -> Result<()> {
    let mut statement = tx.prepare(
        "INSERT OR IGNORE INTO semantic_hierarchy (parent_id, child_id) VALUES (?1, ?2)",
    )?;
    for row in hierarchy.rows() {
        statement.execute(params![
            sqlite_integer(row.parent_id, "parent_id")?,
            sqlite_integer(row.child_id, "child_id")?,
        ])?;
    }
    Ok(())
}

fn create_semantics_table(
    tx: &Transaction<'_>,
    semantics: &SemanticPrimitiveTable<'_>,
    srs_id: i32,
    last_change: &str,
) -> Result<()> {
    let mut columns = vec![
        "id INTEGER PRIMARY KEY".to_string(),
        "cityobject_id TEXT NOT NULL".to_string(),
        "geometry_ix INTEGER NOT NULL".to_string(),
        "semantic_ix INTEGER NOT NULL".to_string(),
        "primitive_ix INTEGER NOT NULL".to_string(),
        "geometry_type TEXT NOT NULL".to_string(),
        "geometry_lod TEXT".to_string(),
        "semantic_type TEXT NOT NULL".to_string(),
        format!("{} GEOMETRY NOT NULL", quote_ident(GPKG_GEOM_COLUMN_NAME)),
    ];
    columns.extend(semantics.schema().columns.iter().map(|column| {
        format!(
            "{} {}",
            quote_ident(&column.name),
            sqlite_type_decl(&column.logical_type)
        )
    }));
    tx.execute_batch(&format!("CREATE TABLE semantics ({});", columns.join(", ")))?;
    tx.execute(
        "INSERT INTO gpkg_contents (
            table_name, data_type, identifier, description, last_change, srs_id
         ) VALUES ('semantics', 'features', 'semantics', 'semantics', ?1, ?2)",
        params![last_change, srs_id],
    )?;
    tx.execute(
        "INSERT INTO gpkg_geometry_columns (
            table_name, column_name, geometry_type_name, srs_id, z, m
         ) VALUES ('semantics', ?1, 'GEOMETRY', ?2, 1, 0)",
        params![GPKG_GEOM_COLUMN_NAME, srs_id],
    )?;
    Ok(())
}

fn insert_semantics_rows(
    tx: &Transaction<'_>,
    semantics: &SemanticPrimitiveTable<'_>,
    srs_id: i32,
) -> Result<Option<[f64; 6]>> {
    let insert_sql = semantic_insert_sql(semantics.schema());
    let mut statement = tx.prepare(&insert_sql)?;
    let mut extent = None;
    for row in semantics.rows() {
        let fixed = row.fixed();
        let Some(semantic_ix) = fixed.semantic_ix else {
            continue;
        };
        let semantic_type = fixed
            .semantic_type_name()
            .map(|semantic_type| semantic_type.to_string())
            .unwrap_or_default();
        let encoded = semantic_primitive_geometry(semantics.model(), fixed)?;
        extent = union_bbox(extent, encoded.bbox);
        let mut params = vec![
            SqlValue::Text(fixed.cityobject_id.to_string()),
            SqlValue::Integer(sqlite_integer(fixed.geometry_ix, "geometry_ix")?),
            SqlValue::Integer(sqlite_integer(semantic_ix, "semantic_ix")?),
            SqlValue::Integer(sqlite_integer(fixed.primitive_ix, "primitive_ix")?),
            SqlValue::Text(fixed.geometry_type.to_string()),
            fixed
                .geometry_lod
                .clone()
                .map_or(SqlValue::Null, SqlValue::Text),
            SqlValue::Text(semantic_type),
            SqlValue::Blob(wrap_geopackage_binary(
                &encoded.wkb,
                encoded.bbox,
                false,
                srs_id,
            )),
        ];
        params.extend(
            row.values()
                .map(|value| sqlite_value_from_tabular_value(semantics.model(), value?))
                .collect::<Result<Vec<_>>>()?,
        );
        statement.execute(params_from_iter(params))?;
    }
    Ok(extent)
}

fn semantic_insert_sql(schema: &crate::TableSchema<'_>) -> String {
    let mut columns = vec![
        quote_ident("cityobject_id"),
        quote_ident("geometry_ix"),
        quote_ident("semantic_ix"),
        quote_ident("primitive_ix"),
        quote_ident("geometry_type"),
        quote_ident("geometry_lod"),
        quote_ident("semantic_type"),
        quote_ident(GPKG_GEOM_COLUMN_NAME),
    ];
    columns.extend(
        schema
            .columns
            .iter()
            .map(|column| quote_ident(&column.name)),
    );
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

fn sqlite_integer(value: u64, column: &str) -> Result<i64> {
    i64::try_from(value)
        .with_context(|| format!("{column} value {value} does not fit in SQLite INTEGER"))
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
    let metadata_json = metadata_to_compact_json(model, metadata)?;
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

fn encode_address_geometry_blob(
    model: &CityModel,
    geometry_handle: cityjson_lib::cityjson_types::resources::handles::GeometryHandle,
    srs_id: i32,
) -> Result<EncodedGeometry> {
    let geometry = model
        .resolve_geometry(geometry_handle)
        .with_context(|| format!("resolve address.location geometry handle {geometry_handle:?}"))?;
    if !matches!(geometry.type_geometry(), GeometryType::MultiPoint) {
        bail!(
            "address.location geometry handle {geometry_handle:?} must resolve to MultiPoint, found {}",
            geometry.type_geometry()
        );
    }
    let Some(boundary) = geometry.boundaries() else {
        bail!("address.location geometry handle {geometry_handle:?} resolves to a geometry without boundaries");
    };
    encode_geometry_blob(model, *geometry.type_geometry(), boundary, srs_id)
}

fn encode_geometry_blob(
    model: &CityModel,
    geometry_type: GeometryType,
    boundary: &Boundary<u32>,
    srs_id: i32,
) -> Result<EncodedGeometry> {
    if matches!(geometry_type, GeometryType::GeometryInstance) {
        bail!("GeometryInstance should have been resolved before GeoPackage export");
    }

    let payload = boundary
        .to_wkb(model.vertices())
        .with_context(|| format!("encode {geometry_type} boundary as WKB"))?;
    let envelope = calculate_envelope(model, boundary)?;

    Ok(EncodedGeometry {
        blob: wrap_geopackage_binary(&payload, envelope, envelope.is_none(), srs_id),
        envelope,
    })
}

#[derive(Debug)]
struct EncodedGeometry {
    blob: Vec<u8>,
    envelope: Option<[f64; 6]>,
}

fn calculate_envelope(model: &CityModel, boundary: &Boundary<u32>) -> Result<Option<[f64; 6]>> {
    let mut envelope = None;
    for vertex_index in boundary.vertices() {
        update_envelope(&mut envelope, vertex_coordinates(model, *vertex_index)?);
    }
    Ok(envelope)
}

fn vertex_coordinates(model: &CityModel, vertex_index: VertexIndex<u32>) -> Result<[f64; 3]> {
    let vertex = model
        .get_vertex(vertex_index)
        .with_context(|| format!("missing vertex {vertex_index}"))?;
    Ok(vertex.to_array())
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
fn wrap_geopackage_binary(
    payload: &[u8],
    envelope: Option<[f64; 6]>,
    empty: bool,
    srs_id: i32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + envelope.map_or(0, |_| 48) + payload.len());
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
    if let Some([min_x, min_y, min_z, max_x, max_y, max_z]) = envelope {
        for value in [min_x, max_x, min_y, max_y, min_z, max_z] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

fn gpkg_geometry_type_name(geometry_type: GeometryType) -> &'static str {
    match geometry_type {
        GeometryType::MultiPoint => "MULTIPOINT",
        GeometryType::MultiLineString => "MULTILINESTRING",
        GeometryType::GeometryInstance => "GEOMETRYCOLLECTION",
        _ => "MULTIPOLYGON",
    }
}

fn layer_table_name_base(
    cityobject_type: &str,
    geometry_family: &str,
    lod: Option<impl ToString>,
    split_lod: bool,
) -> String {
    let mut base = format!(
        "{}_{}",
        sanitize_identifier(cityobject_type),
        sanitize_identifier(geometry_family)
    );
    if split_lod {
        let lod = lod.map_or_else(
            || "none".to_string(),
            |lod| sanitize_lod_fragment(&lod.to_string()),
        );
        base.push_str("_lod");
        base.push_str(&lod);
    }
    base
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
        let candidate = format!("{base}_{index}");
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
        LogicalType::Boolean | LogicalType::UInt64 | LogicalType::Int64 => "INTEGER",
        LogicalType::GeometryRef => "BLOB",
        LogicalType::Float64 => "REAL",
        LogicalType::Utf8
        | LogicalType::Json
        | LogicalType::Null
        | LogicalType::List { .. }
        | LogicalType::Struct(_) => "TEXT",
    }
}

fn sqlite_value_from_tabular_value(model: &CityModel, value: Value<'_, '_>) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Boolean(value) => SqlValue::Integer(i64::from(value)),
        Value::UInt64(value) => match i64::try_from(value) {
            Ok(value) => SqlValue::Integer(value),
            Err(_) => SqlValue::Text(value.to_string()),
        },
        Value::Int64(value) => SqlValue::Integer(value),
        Value::Float64(value) => SqlValue::Real(value),
        Value::Utf8(value) => SqlValue::Text(value.to_string()),
        Value::GeometryRef(value) => SqlValue::Blob(geometry_ref_to_wkb(model, value)?),
        Value::List(values) => {
            let json = value_to_json(model, Value::List(values))?;
            SqlValue::Text(serde_json::to_string(&json)?)
        }
        Value::Struct(values) => {
            let json = value_to_json(model, Value::Struct(values))?;
            SqlValue::Text(serde_json::to_string(&json)?)
        }
        Value::Json(value) => SqlValue::Text(serde_json::to_string(&value_to_json(
            model,
            Value::Json(value),
        )?)?),
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
        let blob = wrap_geopackage_binary(
            &[1, 2, 3],
            Some([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
            false,
            7415,
        );
        assert_eq!(&blob[0..2], b"GP");
        assert_eq!(blob[2], 0);
        assert_eq!(blob[3] & 0b0000_0001, 1);
        assert_eq!(i32::from_le_bytes(blob[4..8].try_into().unwrap()), 7415);
        assert_eq!(blob[3] & 0b0000_0100, 0b0000_0100);
        assert_eq!(blob.len(), 2 + 1 + 1 + 4 + 48 + 3);
    }
}
