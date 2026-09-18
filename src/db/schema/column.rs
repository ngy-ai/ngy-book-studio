//! The column, table and index shape this schema is described with.

#[derive(Clone, Copy)]
pub(crate) struct ColumnSpec {
    pub(crate) name: &'static str,
    pub(crate) data_type: &'static str,
    pub(crate) not_null: bool,
    pub(crate) primary_key: i64,
}

impl ColumnSpec {
    pub(crate) const fn new(
        name: &'static str,
        data_type: &'static str,
        not_null: bool,
        primary_key: i64,
    ) -> Self {
        Self {
            name,
            data_type,
            not_null,
            primary_key,
        }
    }
}

pub(crate) struct TableSpec {
    pub(crate) name: &'static str,
    pub(crate) columns: &'static [ColumnSpec],
}

#[derive(Clone, Copy)]
pub(crate) struct IndexSpec {
    pub(crate) table: &'static str,
    pub(crate) name: &'static str,
    pub(crate) columns: &'static [&'static str],
    pub(crate) unique: bool,
}
