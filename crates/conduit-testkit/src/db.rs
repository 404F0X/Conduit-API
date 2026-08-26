/// The only database backend supported by the active Rust product.
pub const DATABASE_BACKEND: &str = "postgres";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbComparisonKind {
    SchemaCompatibility,
    RowBehaviorCompatibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbTestArea {
    Repository,
    Migration,
    Transaction,
    ProjectFilterAuthPolicy,
    QuotaDateRange,
    RequestPersistence,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbCompatibilityCase {
    pub name: String,
    pub kind: DbComparisonKind,
    pub area: DbTestArea,
}

impl DbCompatibilityCase {
    pub fn new(name: impl Into<String>, kind: DbComparisonKind, area: DbTestArea) -> Self {
        Self {
            name: name.into(),
            kind,
            area,
        }
    }

    pub const fn database_backend(&self) -> &'static str {
        DATABASE_BACKEND
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_persistence_uses_postgres() {
        let case = DbCompatibilityCase::new(
            "request rows",
            DbComparisonKind::RowBehaviorCompatibility,
            DbTestArea::RequestPersistence,
        );

        assert_eq!(case.database_backend(), "postgres");
    }
}
