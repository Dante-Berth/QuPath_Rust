//! Writes the GeoJSON sample the QuPath-side parity recorder reads.
//!
//! `docs/VIEWER.md` §8.8's last item: both file formats had been checked
//! against a JSON parser and against the Java that writes them, and neither had
//! ever been opened *by QuPath*. This produces the file that gets opened —
//! deterministically, so `parity/java/GeoJsonImport.java` records QuPath's
//! reading of a fixed input and `tests/parity_geojson_import.rs` can assert the
//! two agree with no JVM in the loop.
//!
//! ```text
//! cargo run -p qupath-viewer --example geojson_sample          # parity/samples/
//! cargo run -p qupath-viewer --example geojson_sample -- /tmp/x.geojson
//! ```
//!
//! Run this after any change to `geojson::write`, then re-record:
//! `parity_geojson_import` fails until the committed sample matches the writer,
//! and the fixture beside it is QuPath's reading of the *old* file until
//! `./parity/record.sh` is run again.
//!
//! What the sample deliberately contains, and why each case is in it, is in
//! `examples/shared/sample.rs` beside the construction itself.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

#[path = "shared/sample.rs"]
mod sample;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| sample::PATH.to_owned());
    let (text, written) = sample::write();

    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent).expect("sample directory");
    }
    std::fs::write(&path, &text).expect("write the sample");
    println!(
        "wrote {written} of {} objects, {} bytes, to {path}",
        sample::overlay().len(),
        text.len()
    );
}
