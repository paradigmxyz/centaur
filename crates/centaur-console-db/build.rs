// `sqlx::migrate!` embeds the migrations at compile time but tracks only the
// files that existed then; rebuild when one is added.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
