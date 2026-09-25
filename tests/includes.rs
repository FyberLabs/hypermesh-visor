use std::process::Command;

#[test]
fn every_include_path_exists() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("python3")
        .arg(root.join("ci/check-includes.py"))
        .current_dir(&root)
        .status()
        .expect("python3 runs ci/check-includes.py");
    assert!(status.success(), "an include_str! or include_bytes! path is missing");
}
