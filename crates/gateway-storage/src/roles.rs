//! The least-privilege roles an operator applies, shipped with the code that
//! depends on them.
//!
//! The SQL lives in `db/roles/` so it can be applied with `psql`; it is also
//! embedded here so the scenario that proves the privileges cannot drift from
//! the files an operator will run.

/// The group roles, in the order they are applied.
pub const ROLES_SQL: &str = include_str!("../../../db/roles/00_roles.sql");

/// The table privileges, applied after every migration.
pub const GRANTS_SQL: &str = include_str!("../../../db/roles/10_grants.sql");

#[cfg(test)]
mod tests;
