//! PostgreSQL files without a database: PostgreSQL 18's own parser (libpg_query through
//! `pg_query`) reads `.sql` files, highlights them and finds their objects, references and
//! syntax errors; [`Index`] links the files of a folder.
//!
//! Modelled on PG Studio (ambit-project/ambit-pg-studio): the same parser, Liquibase formatted
//! SQL, PL/pgSQL bodies read token by token. Offsets are bytes of the file.

mod analyze;
mod body;
pub mod ffi;
pub mod highlight;
mod index;
pub mod model;
pub mod prepare;
pub mod tokens;

pub use analyze::analyze;
pub use highlight::{HlKind, highlight};
pub use index::{Hit, Index, Location};
pub use model::*;

/// `.sql`, `.pgsql` and `.psql` files.
pub fn is_sql_path(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["sql", "pgsql", "psql"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

// The C library is linked through this crate even where only `ffi` calls into it.
extern crate pg_query;
