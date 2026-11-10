//! Endpoint identity shared by the companion and its independent Windows host.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub fn pipe_name(sid: &str, executable: &Path) -> Result<String> {
    let executable = std::fs::canonicalize(executable)
        .context("Could not resolve this companion's executable")?;
    let mut hash = Sha256::new();
    hash.update(sid.as_bytes());
    hash.update([0]);
    hash.update(executable.as_os_str().as_encoded_bytes());
    hash.update([0]);
    let mut file = File::open(&executable).context("Could not identify this companion's build")?;
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let size = file.read(&mut bytes)?;
        if size == 0 {
            break;
        }
        hash.update(&bytes[..size]);
    }
    Ok(format!(
        r"\\.\pipe\gridstator-worker-host-v2-{:x}",
        hash.finalize()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updated_installation_does_not_connect_to_a_portable_previews_host() -> Result<()> {
        let root = tempfile::tempdir()?;
        let preview = root
            .path()
            .join("Downloads/old preview/gridstator-desktop.exe");
        let installed = root.path().join("Gridstator/gridstator-desktop.exe");
        for path in [&preview, &installed] {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, b"same desktop binary")?;
        }
        let sid = "S-1-5-21-1234";
        assert_ne!(
            pipe_name(sid, &preview)?,
            pipe_name(sid, &installed)?,
            "The installed companion must not route setup to the Downloads preview's host"
        );
        let original = pipe_name(sid, &installed)?;
        assert_eq!(original, pipe_name(sid, &installed)?);
        std::fs::write(&installed, b"updated desktop with a bundled native CLI")?;
        assert_ne!(
            original,
            pipe_name(sid, &installed)?,
            "An in-place update must not connect to a host running the previous executable"
        );
        assert_ne!(
            pipe_name(sid, &installed)?,
            pipe_name("S-1-5-21-5678", &installed)?
        );
        Ok(())
    }
}
