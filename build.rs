//! Packs the licenses shown in About with deflate: the open source licenses (`assets/licenses.json`, from `scripts/update-licenses.sh`), whose texts and notices repeat a lot, so the binary carries about a sixth of the JSON, and Syncative's own `LICENSE`.

fn main() {
    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR");
    for (source, packed) in [("assets/licenses.json", "licenses.json.deflate"), ("LICENSE", "license.deflate")] {
        println!("cargo:rerun-if-changed={source}");
        let data = std::fs::read(source).expect("read the license file");
        std::fs::write(std::path::Path::new(&out_dir).join(packed), miniz_oxide::deflate::compress_to_vec(&data, 10)).expect("write the packed license");
    }
}
