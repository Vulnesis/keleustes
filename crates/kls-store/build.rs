fn main() {
    // sqlx::migrate! embeds the folders at compile time.
    println!("cargo:rerun-if-changed=migrations");
}
