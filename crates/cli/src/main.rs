//! File Checker CLI entry point.
//!
//! Subcommands will be added per `docs/requirements.md` §10.16 as the
//! corresponding core functionality lands (see `docs/implementation-plan.md`).

fn main() {
    println!("filechecker-cli {}", filechecker_core::version());
}
