//! Database persistence layer for the Tessera API.
//!
//! - [`migrator`] — the zero-downtime migration engine (issue #71).
//! - [`embedded_migrations`] — the compiled-in migration set, so the
//!   distroless container (issue #26) can run `tessera-api migrate` with no
//!   filesystem access to the SQL files.

pub mod migrator;

use migrator::Migration;

/// The migration files under `src/db/migrations/`, embedded into the binary.
///
/// Versions must be unique and strictly increasing; the file name after the
/// version prefix becomes the migration name. A migration is a *contract
/// phase* when its name contains `contract` (case-insensitive) — see
/// [`migrator`] for what that gates. The `-- transactional` first-line header
/// opts a file into single-transaction execution.
pub fn embedded_migrations() -> Vec<Migration> {
    let migrations = vec![
        Migration::new(1, "initial", include_str!("migrations/0001_initial.sql"))
            .with_down(include_str!("migrations/0001_initial.down.sql")),
        Migration::new(
            2,
            "expand_holder_balance_minor",
            include_str!("migrations/0002_expand_holder_balance_minor.sql"),
        )
        .with_down(include_str!(
            "migrations/0002_expand_holder_balance_minor.down.sql"
        )),
        Migration::new(
            3,
            "contract_holder_balance_minor",
            include_str!("migrations/0003_contract_holder_balance_minor.sql"),
        )
        .with_down(include_str!(
            "migrations/0003_contract_holder_balance_minor.down.sql"
        )),
    ];
    // The literal set above is compile-time known; validation only guards
    // against editing mistakes (duplicate/zero versions).
    migrator::load_embedded(&migrations).expect("embedded migration set is well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_set_is_well_formed() {
        let migrations = embedded_migrations();
        assert_eq!(migrations.len(), 3);

        let versions: Vec<i64> = migrations.iter().map(|m| m.version).collect();
        assert_eq!(versions, vec![1, 2, 3]);

        // Every migration ships a rollback script (issue #71 acceptance
        // criterion: "rollback scripts are verified for every step").
        for m in &migrations {
            assert!(
                m.down_sql.is_some(),
                "migration {} is missing a .down.sql",
                m
            );
        }
    }

    #[test]
    fn phase_flags_match_file_names() {
        let migrations = embedded_migrations();
        assert!(!migrations[0].is_contract_phase);
        assert!(!migrations[1].is_contract_phase);
        assert!(migrations[2].is_contract_phase);
    }
}
