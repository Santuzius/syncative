//! Packs the open source licenses shown in About (`assets/licenses.json`, from `scripts/update-licenses.sh`) with deflate: the license texts and notices repeat a lot, so the binary carries about a sixth of the JSON.

fn main() {
    println!("cargo:rerun-if-changed=assets/licenses.json");
    let json = std::fs::read("assets/licenses.json").expect("read assets/licenses.json");
    let out = std::path::Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("licenses.json.deflate");
    std::fs::write(out, miniz_oxide::deflate::compress_to_vec(&json, 10)).expect("write the packed licenses");
}
