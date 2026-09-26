// Rebuild when the embedded inputs change: the SQL migrations (sqlx::migrate!)
// and the built web UI (rust-embed). Neither macro tells cargo about them.
fn main() {
    println!("cargo:rerun-if-changed=../sql/migrations");
    println!("cargo:rerun-if-changed=../frontend/dist");
}
