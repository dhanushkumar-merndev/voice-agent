//! Rebuilds this crate when a migration is added or changed, so
//! `sqlx::migrate!` in `db.rs` always embeds every migration.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
