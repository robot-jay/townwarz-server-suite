//! PostgreSQL boundary. ORM entities and SQL stay inside this module;
//! callers receive domain records, never ActiveModels or database connections.
mod entities;
mod repository;

pub(crate) use repository::{GameAdmission, GameRepository};

#[cfg(test)]
mod tests;
