use anyhow::{Result, ensure};
use gridstator::bridge;
use serde_json::json;

#[tokio::test]
async fn source_snapshot_013_git_root_relative_workdir_and_nearest_configuration() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let repo = directory.path().join("repository");
    let nested = repo.join("packages/a/src");
    std::fs::create_dir_all(&nested)?;
    ensure!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()?
            .success(),
        "git init failed"
    );
    let request = json!({"version":1,"operation":"discover","root":nested});
    let discovered = bridge::execute(request.clone()).await?;
    assert_eq!(discovered["root"], json!(std::fs::canonicalize(&repo)?));
    assert_eq!(discovered["workdir"], "packages/a/src");
    assert!(discovered.get("configPath").is_none());
    let configured = repo.join("packages/a");
    std::fs::write(
        configured.join("gridstator.yml"),
        "project: a\ntasks:\n  t:\n    command: echo hi\n",
    )?;
    let discovered = bridge::execute(request).await?;
    assert_eq!(
        discovered["root"],
        json!(std::fs::canonicalize(&configured)?)
    );
    assert_eq!(discovered["workdir"], "src");
    assert_eq!(discovered["config"]["project"], "a");
    assert_eq!(discovered["config"]["tasks"]["t"]["command"], "echo hi");
    assert_eq!(
        discovered["configPath"],
        json!(std::fs::canonicalize(configured.join("gridstator.yml"))?)
    );
    Ok(())
}

#[tokio::test]
async fn source_snapshot_014_invalid_configuration_names_the_configuration_file() -> Result<()> {
    let directory = tempfile::tempdir()?;
    std::fs::write(directory.path().join("gridstator.yml"), "bogus: 1\n")?;
    let error =
        bridge::execute(json!({"version":1,"operation":"discover","root":directory.path()}))
            .await
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("Invalid gridstator.yml"),
        "{error:#}"
    );
    Ok(())
}
