//! Enables the `db_tests` cfg when `DATABASE_URL` is set, so the PostgreSQL
//! integration tests run with a database and are reported as ignored, with a
//! reason, without one. `vm_tests` does the same for a real VictoriaMetrics
//! at `VICTORIAMETRICS_URL`.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(db_tests)");
    println!("cargo::rustc-check-cfg=cfg(vm_tests)");
    println!("cargo::rerun-if-env-changed=DATABASE_URL");
    println!("cargo::rerun-if-env-changed=VICTORIAMETRICS_URL");
    let set = |var| std::env::var(var).is_ok_and(|url| !url.is_empty());
    if set("DATABASE_URL") {
        println!("cargo::rustc-cfg=db_tests");
    }
    if set("VICTORIAMETRICS_URL") {
        println!("cargo::rustc-cfg=vm_tests");
    }
}
