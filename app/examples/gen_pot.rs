//! Regenerate the translation template (app/po/blindrss.pot)
//! from the strings marked with `t()` / `nt()` in this app and
//! its translatable libraries. Maintainer tool, run with:
//! `cargo run --example gen_pot` (needs the gettext tools).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    patois_build::gen_pot(".", "po", "blindrss")
}
