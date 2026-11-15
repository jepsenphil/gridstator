use anyhow::Result;
use gridstator::{packaging, release, updates};
use serde_json::json;
use std::{fs, path::Path};

fn inputs(root: &Path, version: &str) -> Result<()> {
    inputs_for(root, version, &release::SUPPORTED_TARGETS)
}
fn inputs_for(root: &Path, version: &str, targets: &[&str]) -> Result<()> {
    for &target in targets {
        let directory = root.join(target.replace('/', "-"));
        fs::create_dir_all(&directory)?;
        let mut bytes = [0u8; 128];
        if target.starts_with("linux/") {
            bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
            bytes[18..20].copy_from_slice(
                &(if target.ends_with("amd64") {
                    62u16
                } else {
                    183u16
                })
                .to_le_bytes(),
            );
        } else if target.starts_with("windows/") {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64_u32.to_le_bytes());
            bytes[64..68].copy_from_slice(b"PE\0\0");
            let machine: u16 = if target.ends_with("amd64") {
                0x8664
            } else {
                0xaa64
            };
            bytes[68..70].copy_from_slice(&machine.to_le_bytes());
            bytes[84..86].copy_from_slice(&2_u16.to_le_bytes());
            bytes[86..88].copy_from_slice(&2_u16.to_le_bytes());
            bytes[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
        } else {
            bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            bytes[4..8].copy_from_slice(
                &(if target.ends_with("amd64") {
                    0x1000007u32
                } else {
                    0x100000cu32
                })
                .to_le_bytes(),
            );
        }
        let executable = directory.join(if target.starts_with("windows/") {
            "gridstator.exe"
        } else {
            "gridstator"
        });
        fs::write(&executable, bytes)?;
        let (size, hash) = release::hash_file(&executable)?;
        fs::write(
            directory.join("build-info.json"),
            serde_json::to_vec(
                &json!({"target":target,"version":version,"buildId":"candidate-123","sourceRevision":"a".repeat(40),"size":size,"sha256":hash}),
            )?,
        )?;
    }
    Ok(())
}
#[test]
fn stable_linux_pair_and_unsigned_mac_preview_have_distinct_manifests() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let linux_input = temp.path().join("linux-input");
    inputs_for(&linux_input, "2.0.0", &["linux/amd64", "linux/arm64"])?;
    let linux_output = temp.path().join("linux-release");
    let stable = packaging::assemble(&linux_input, &linux_output, false)?;
    assert_eq!(stable.artifacts.len(), 2);
    assert!(stable.target("darwin/arm64").is_err());
    let bundle = linux_output.join("gridstator-2.0.0-native-bundle.tar.gz");
    let (_, hash) = release::hash_file(&bundle)?;
    assert_eq!(
        updates::import_bundle(&bundle, &hash, &temp.path().join("controller"))?,
        stable
    );

    let mac_input = temp.path().join("mac-input");
    inputs_for(&mac_input, "2.0.0-macos-preview.1", &["darwin/arm64"])?;
    let preview = packaging::assemble(&mac_input, &temp.path().join("mac-preview"), false)?;
    assert_eq!(
        preview.artifacts.keys().collect::<Vec<_>>(),
        vec!["darwin/arm64"]
    );
    inputs_for(&mac_input, "2.0.0", &["darwin/arm64"])?;
    assert!(packaging::assemble(&mac_input, &temp.path().join("invalid-stable"), false).is_err());
    Ok(())
}
#[test]
fn deterministic_native_archives_and_bundle_are_consumable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    inputs(&input, "2.0.0-rc.1")?;
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let manifest = packaging::assemble(&input, &first, false)?;
    assert_eq!(packaging::assemble(&input, &second, false)?, manifest);
    assert_eq!(
        fs::read(first.join("SHA256SUMS"))?,
        fs::read(second.join("SHA256SUMS"))?
    );
    let installer = first.join("install-native.sh");
    assert_eq!(
        fs::read(&installer)?,
        include_bytes!("../../scripts/install-native.sh")
    );
    assert_eq!(
        release::checksum_for(
            &fs::read_to_string(first.join("SHA256SUMS"))?,
            "install-native.sh"
        )?,
        release::hash_file(&installer)?.1
    );
    let bundle = first.join("gridstator-2.0.0-rc.1-native-bundle.tar.gz");
    let (_, hash) = release::hash_file(&bundle)?;
    let imported = updates::import_bundle(&bundle, &hash, &temp.path().join("controller"))?;
    assert_eq!(imported, manifest);
    for (target, artifact) in &manifest.artifacts {
        let archive = first.join(&artifact.archive.file);
        artifact.archive.verify(&archive)?;
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(archive)?));
        let names = tar
            .entries()?
            .map(|entry| Ok(entry?.path()?.into_owned()))
            .collect::<Result<Vec<_>>>()?;
        let executable_name = if target.starts_with("windows/") {
            "gridstator.exe"
        } else {
            "gridstator"
        };
        assert_eq!(
            names,
            vec![Path::new(executable_name), Path::new("LICENSE")]
        );
        assert_eq!(
            artifact.executable,
            release::FileDigest::from_path(
                executable_name,
                input.join(target.replace('/', "-")).join(executable_name)
            )?
        );
    }
    Ok(())
}
#[test]
fn stable_release_requires_both_mac_receipts_and_final_signed_hashes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    inputs(&input, "2.0.0")?;
    let output = temp.path().join("release");
    assert!(packaging::assemble(&input, &output, false).is_err());
    assert!(!output.exists());
    for target in ["darwin-amd64", "darwin-arm64"] {
        fs::write(
            input.join(target).join("notarization.json"),
            br#"{"id":"submission-id","status":"Accepted"}"#,
        )?;
    }
    // A codesign mutation after metadata collection must fail, even with a successful receipt.
    let binary = input.join("darwin-arm64/gridstator");
    let mut changed = fs::read(&binary)?;
    changed.push(1);
    fs::write(&binary, changed)?;
    assert!(packaging::assemble(&input, &output, true).is_err());
    assert!(!output.exists());
    Ok(())
}
#[test]
fn mixed_build_identity_or_architecture_cannot_produce_a_manifest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    inputs(&input, "2.0.0-dev.1")?;
    let path = input.join("linux-arm64/build-info.json");
    let mut info: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    info["buildId"] = json!("another-build");
    fs::write(&path, serde_json::to_vec(&info)?)?;
    assert!(packaging::assemble(&input, &temp.path().join("bad"), false).is_err());
    inputs(&input, "2.0.0-dev.1")?;
    fs::copy(
        input.join("linux-amd64/gridstator"),
        input.join("linux-arm64/gridstator"),
    )?;
    assert!(packaging::assemble(&input, &temp.path().join("bad"), false).is_err());
    Ok(())
}
