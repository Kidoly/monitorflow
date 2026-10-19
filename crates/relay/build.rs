//! Enables the `db_tests` cfg when `DATABASE_URL` is set, so the PostgreSQL
//! integration tests run with a database and are reported as ignored, with a
//! reason, without one.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(db_tests)");
    println!("cargo::rerun-if-env-changed=DATABASE_URL");
    if std::env::var("DATABASE_URL").is_ok_and(|url| !url.is_empty()) {
        println!("cargo::rustc-cfg=db_tests");
    }
}
