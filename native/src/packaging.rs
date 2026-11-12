//! Build-host release assembly. Target installations only consume its verified artifacts.
use crate::release::{self, FileDigest, ProtocolCompatibility, ReleaseArtifact, ReleaseManifest};
use anyhow::{Context, Result, ensure};
use flate2::{Compression, GzBuilder};
use serde_json::Value;
use std::{collections::BTreeMap, fs, io::Write, path::Path};

fn append(
    builder: &mut tar::Builder<impl Write>,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)?;
    Ok(())
}
fn digest(file: &str, path: &Path) -> Result<FileDigest> {
    Ok(FileDigest::from_path(file, path)?)
}
/// Each <os>-<arch> input contains the final executable and its actual-host build-info.json.
/// macOS hashes must be generated after signing. No release is published by this operation.
pub fn assemble(
    input: &Path,
    output: &Path,
    require_notarization: bool,
) -> Result<ReleaseManifest> {
    ensure!(!output.exists(), "release output already exists");
    let parent = output.parent().context("release output needs a parent")?;
    fs::create_dir_all(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let mut identity: Option<(String, String, String)> = None;
    let mut artifacts = BTreeMap::new();
    for target in release::SUPPORTED_TARGETS {
        let directory = input.join(target.replace('/', "-"));
        if !directory.exists() {
            continue;
        }
        let binary = directory.join("gridstator");
        ensure!(
            fs::symlink_metadata(&binary)?.file_type().is_file(),
            "release executable must be a regular file"
        );
        release::verify_executable_target(&binary, target)?;
        let info: Value = serde_json::from_slice(&fs::read(directory.join("build-info.json"))?)?;
        ensure!(info["target"] == target, "build metadata target mismatch");
        let version = info["version"]
            .as_str()
            .context("missing release version")?
            .to_owned();
        let parsed = semver::Version::parse(&version)?;
        ensure!(parsed.major == 2, "native release version must be v2");
        let build_id = info["buildId"]
            .as_str()
            .context("missing build identity")?
            .to_owned();
        let revision = info["sourceRevision"]
            .as_str()
            .context("missing source revision")?
            .to_owned();
        ensure!(
            revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "release requires a full source revision"
        );
        let current = (version.clone(), build_id, revision);
        if let Some(expected) = &identity {
            ensure!(
                &current == expected,
                "all artifacts must have the same version, build ID and source revision"
            );
        } else {
            identity = Some(current);
        }
        if target.starts_with("darwin/") && (require_notarization || parsed.pre.is_empty()) {
            let receipt: Value = serde_json::from_slice(
                &fs::read(directory.join("notarization.json"))
                    .context("macOS release requires a notarization receipt")?,
            )?;
            ensure!(
                receipt["status"] == "Accepted"
                    && receipt["id"].as_str().is_some_and(|id| !id.is_empty()),
                "macOS notarization did not succeed"
            );
        }
        let executable = digest("gridstator", &binary)?;
        ensure!(
            info["sha256"] == executable.sha256 && info["size"] == executable.size,
            "build metadata must describe final signed executable bytes"
        );
        let name = format!("gridstator-{version}-{}.tar.gz", target.replace('/', "-"));
        let path = staging.path().join(&name);
        let gzip = GzBuilder::new()
            .mtime(0)
            .write(fs::File::create(&path)?, Compression::default());
        let mut archive = tar::Builder::new(gzip);
        append(&mut archive, "gridstator", &fs::read(binary)?, 0o755)?;
        append(
            &mut archive,
            "LICENSE",
            include_bytes!("../../LICENSE"),
            0o644,
        )?;
        archive.into_inner()?.finish()?.sync_all()?;
        artifacts.insert(
            target.to_owned(),
            ReleaseArtifact {
                archive: digest(&name, &path)?,
                executable,
            },
        );
    }
    let (version, build_id, source_revision) = identity.context("no release artifacts")?;
    if semver::Version::parse(&version)?.pre.is_empty() {
        ensure!(
            artifacts.contains_key("linux/amd64") && artifacts.contains_key("linux/arm64"),
            "stable release requires both Linux targets"
        );
        ensure!(
            artifacts.keys().all(|target| target.starts_with("linux/"))
                || release::SUPPORTED_TARGETS
                    .iter()
                    .all(|target| artifacts.contains_key(*target)),
            "stable release cannot include only one macOS architecture"
        );
    }
    let manifest = ReleaseManifest {
        schema_version: 1,
        build_id,
        version,
        source_revision,
        protocol: ProtocolCompatibility::default(),
        artifacts,
    };
    manifest.validate()?;
    fs::write(
        staging.path().join("install-native.sh"),
        include_bytes!("../../scripts/install-native.sh"),
    )?;
    let manifest_path = staging.path().join(release::RELEASE_MANIFEST_NAME);
    fs::write(&manifest_path, manifest.to_bytes()?)?;
    let bundle_name = format!("gridstator-{}-native-bundle.tar.gz", manifest.version);
    let bundle_path = staging.path().join(&bundle_name);
    let gzip = GzBuilder::new()
        .mtime(0)
        .write(fs::File::create(&bundle_path)?, Compression::default());
    let mut bundle = tar::Builder::new(gzip);
    append(
        &mut bundle,
        release::RELEASE_MANIFEST_NAME,
        &fs::read(&manifest_path)?,
        0o644,
    )?;
    for artifact in manifest.artifacts.values() {
        append(
            &mut bundle,
            &artifact.archive.file,
            &fs::read(staging.path().join(&artifact.archive.file))?,
            0o644,
        )?;
    }
    bundle.into_inner()?.finish()?.sync_all()?;
    let mut names = manifest
        .artifacts
        .values()
        .map(|artifact| artifact.archive.file.clone())
        .collect::<Vec<_>>();
    names.extend([
        release::RELEASE_MANIFEST_NAME.to_owned(),
        bundle_name,
        "install-native.sh".to_owned(),
    ]);
    names.sort();
    let mut checksums = String::new();
    for name in names {
        let (_, hash) = release::hash_file(staging.path().join(&name))?;
        checksums.push_str(&format!("{hash}  {name}\n"));
    }
    fs::write(staging.path().join("SHA256SUMS"), checksums)?;
    fs::rename(staging.path(), output)?;
    Ok(manifest)
}
