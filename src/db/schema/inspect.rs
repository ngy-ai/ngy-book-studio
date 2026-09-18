//! Reading back what the database actually contains.

use super::*;

pub(crate) fn pragma_i64(conn: &Connection, name: &str) -> Result<i64> {
    conn.pragma_query_value(None, name, |row| row.get(0))
        .with_context(|| format!("无法读取数据库 {name}"))
}

pub(crate) fn object_exists(
    conn: &Connection,
    object_type: &str,
    name: &str,
    table: &str,
) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_schema WHERE type = ?1 AND name = ?2 AND tbl_name = ?3",
        (object_type, name, table),
        |_| Ok(()),
    )
    .optional()
    .context("无法检查数据库对象")
    .map(|value| value.is_some())
}

pub(crate) fn table_matches(
    conn: &Connection,
    table: &str,
    expected: &[ColumnSpec],
) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("无法检查数据表 {table}"))?;
    let actual = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .with_context(|| format!("无法读取数据表 {table} 的字段"))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法读取数据表 {table} 的字段"))?;
    Ok(actual.len() == expected.len()
        && actual.iter().zip(expected).all(
            |((name, data_type, not_null, primary_key), expected)| {
                name == expected.name
                    && data_type.eq_ignore_ascii_case(expected.data_type)
                    && *not_null == expected.not_null
                    && *primary_key == expected.primary_key
            },
        ))
}

pub(crate) fn index_matches(conn: &Connection, expected: &IndexSpec) -> Result<bool> {
    let mut index_list = conn
        .prepare(&format!("PRAGMA index_list({})", expected.table))
        .with_context(|| format!("无法检查数据表 {} 的索引", expected.table))?;
    let indexes = index_list
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })
        .with_context(|| format!("无法读取数据表 {} 的索引", expected.table))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法读取数据表 {} 的索引", expected.table))?;
    let Some((_, unique, origin, partial)) =
        indexes.iter().find(|(name, ..)| name == expected.name)
    else {
        return Ok(false);
    };
    if *unique != expected.unique || origin != "c" || *partial {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_info({})", expected.name))
        .with_context(|| format!("无法检查数据库索引 {}", expected.name))?;
    let actual = stmt
        .query_map([], |row| row.get::<_, String>(2))
        .with_context(|| format!("无法读取数据库索引 {}", expected.name))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("无法读取数据库索引 {}", expected.name))?;
    Ok(actual
        .iter()
        .map(String::as_str)
        .eq(expected.columns.iter().copied()))
}
