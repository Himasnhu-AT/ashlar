//! Tracer server: ingest API, storage, rollups, dashboard and reports in one binary.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// Schema migrations, applied in order. Add a new file; never edit one that has shipped.
pub const MIGRATIONS: &[(&str, &str)] = &[("0001_init", include_str!("../migrations/0001_init.sql"))];

fn main() {
    eprintln!("tracer-server {}: not implemented yet", env!("CARGO_PKG_VERSION"));
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrations_apply_to_an_empty_database() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for (name, sql) in super::MIGRATIONS {
            db.execute_batch(sql).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        let tables: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |row| row.get(0)).unwrap();
        assert!(tables >= 18, "{tables} tables");
    }
}
