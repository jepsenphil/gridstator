use gridstator::discovery::*;
use serde_json::{Value, json};
use std::{fs, path::Path};
fn write(root: &Path, path: &str, text: &str) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}
#[test]
fn workflow_events_inputs_and_warnings_match_reference() {
    let workflow = parse_workflow(
        ".github/workflows/ci.yml",
        r#"name: CI
on:
  pull_request:
  workflow_dispatch:
    inputs:
      env: { description: Target, required: true, type: choice, options: [dev, prod] }
      dry: { default: false }
permissions: { id-token: write }
jobs:
  test: { runs-on: ubuntu-latest, strategy: { matrix: { node: [20, 22] } }, steps: [] }
  deploy: { needs: [test], runs-on: ubuntu-latest, environment: production }
  win: { runs-on: windows-latest }
  remote: { uses: org/repo/.github/workflows/build.yml@main }
  local: { uses: ./.github/workflows/build.yml }
  docker:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { repository: org/tools }
      - uses: docker/build-push-action@v6
      - run: echo "${{ secrets.GITHUB_TOKEN }}"
"#,
    );
    assert_eq!(workflow["name"], "CI");
    assert_eq!(
        workflow["events"],
        json!(["pull_request", "workflow_dispatch"])
    );
    assert_eq!(workflow["defaultEvent"], "workflow_dispatch");
    assert_eq!(
        workflow["dispatchInputs"],
        json!([{"name":"env","description":"Target","required":true,"type":"choice","options":["dev","prod"]},{"name":"dry","required":false,"default":"false"}])
    );
    assert_eq!(workflow["jobs"][0]["matrix"], true);
    assert_eq!(workflow["jobs"][0]["id"], "test");
    assert_eq!(workflow["jobs"][0]["needs"], json!([]));
    assert_eq!(workflow["jobs"][0]["runnable"], true);
    assert_eq!(workflow["jobs"][1]["id"], "deploy");
    assert_eq!(workflow["jobs"][1]["matrix"], false);
    assert_eq!(workflow["jobs"][1]["runnable"], true);
    assert_eq!(workflow["jobs"][1]["needs"], json!(["test"]));
    assert!(
        workflow["jobs"][1]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("deployment environments")
    );
    assert_eq!(workflow["jobs"][2]["runnable"], false);
    assert!(
        workflow["jobs"][3]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("remote reusable workflow")
    );
    assert_eq!(workflow["jobs"][4]["warnings"], json!([]));
    assert!(
        workflow["jobs"][5]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("clones that repository")
    );
    assert!(
        workflow["jobs"][5]["warnings"][1]
            .as_str()
            .unwrap()
            .contains("Docker socket")
    );
    assert_eq!(workflow["warnings"].as_array().unwrap().len(), 2);
    let warnings = workflow["warnings"].to_string();
    assert!(warnings.contains("GITHUB_TOKEN"));
    assert!(warnings.contains("OIDC"));
    assert!(workflow.get("parseError").is_none());
}
#[test]
fn walk_order_exclusions_managers_stages_and_tasks_are_preserved() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(
        root,
        "package.json",
        r#"{"name":"root","scripts":{"test":"vitest","ignored":false},"packageManager":"pnpm@10.0.0"}"#,
    );
    write(
        root,
        "packages/web/package.json",
        r#"{"name":"web","scripts":{"build":"next build"}}"#,
    );
    write(
        root,
        "node_modules/x/package.json",
        r#"{"scripts":{"bad":"true"}}"#,
    );
    write(
        root,
        ".hidden/package.json",
        r#"{"scripts":{"bad":"true"}}"#,
    );
    write(
        root,
        "Dockerfile",
        "FROM node:24 AS deps\nFROM deps as build\nFROM --platform=$BUILDPLATFORM nginx:alpine AS runner\n",
    );
    write(root, "services/api/api.Dockerfile", "FROM python:3.13\n");
    write(
        root,
        ".github/workflows/a.yml",
        "on: push\njobs: {t: {runs-on: ubuntu-latest, steps: []}}\n",
    );
    write(root, ".github/workflows/bad.yaml", "on: [push\n");
    write(root, ".github/workflows/readme.md", "ignored");
    write(
        root,
        "gridstator.yml",
        "tasks:\n  check:\n    command: [echo, ok]\n    retry: {}\n",
    );
    let found = discover(root).unwrap();
    assert_eq!(
        found["packages"],
        json!([{"dir":".","name":"root","packageManager":"pnpm","scripts":{"test":"vitest"}},{"dir":"packages/web","name":"web","packageManager":"pnpm","scripts":{"build":"next build"}}])
    );
    assert_eq!(
        found["dockerfiles"],
        json!([{"file":"Dockerfile","context":".","stages":["deps","build","runner"],"baseImages":["node:24","deps","nginx:alpine"]},{"file":"services/api/api.Dockerfile","context":"services/api","stages":[],"baseImages":["python:3.13"]}])
    );
    assert!(found["workflows"][1]["parseError"].is_string());
    let workflows = found["workflows"].as_array().unwrap();
    assert_eq!(
        workflows
            .iter()
            .map(|workflow| workflow["file"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [".github/workflows/a.yml", ".github/workflows/bad.yaml"]
    );
    assert_eq!(
        find_workflow(workflows, "a.yml").unwrap()["file"],
        ".github/workflows/a.yml"
    );
    assert!(find_workflow(workflows, "./.github/workflows/a.yml").is_some());
    assert_eq!(
        found["tasks"]["check"]["retry"],
        json!({"max":0,"on":["lost"]})
    );
    assert_eq!(
        script_command("pnpm", "build", "packages/web").unwrap(),
        "corepack enable && pnpm install --frozen-lockfile && cd \"packages/web\" && pnpm run \"build\""
    );
    assert_eq!(
        script_command("npm", "test", ".").unwrap(),
        "npm ci && npm run \"test\""
    );
    assert_eq!(
        parse_dockerfile("FROM a AS b\n# FROM c AS d\n")["stages"],
        json!(["b"])
    );
}
#[test]
fn frozen_project_defaults_and_strictness_match() {
    let fixture: Value =
        serde_json::from_str(include_str!("../contracts/accepted-v1/schema-cases.json")).unwrap();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["schema"] == "ProjectConfig")
    {
        let result = normalize_project_config(case["input"].clone());
        if case["success"] == true {
            assert_eq!(result.unwrap(), case["output"], "{}", case["id"]);
        } else {
            assert!(result.is_err(), "{}", case["id"]);
        }
    }
}
#[test]
fn local_lockfiles_override_root_fallback_and_symlinks_are_not_walked() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    write(root, "package.json", r#"{"scripts":{"test":"true"}}"#);
    write(root, "pnpm-lock.yaml", "");
    write(root, "web/package.json", r#"{"scripts":{"build":"true"}}"#);
    write(root, "web/yarn.lock", "");
    write(root, "broken/package.json", "{invalid");
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("web"), root.join("linked")).unwrap();
    let packages = discover_package_scripts(root).unwrap();
    assert_eq!(packages.len(), 2);
    assert_eq!(packages[0]["packageManager"], "pnpm");
    assert_eq!(packages[1]["packageManager"], "yarn");
}
#[test]
fn project_yaml_whole_numbers_match_javascript_normalization() {
    let yaml = r#"version: 1.0
defaults:
  resources: { cpus: 2.0, memory: 4096.0 }
snapshot: { maxFileSize: 1.0 }
tasks:
  check:
    command: echo test
    resources: { cpus: 1.5, memory: 512.0 }
    priority: -2.0
    shards: 2.0
    retry: { max: 2.0 }
"#;
    // This expected value was replayed through the frozen v1 yaml parser and
    // ProjectConfigSchema at 4b8f13ccf1b21820007fef0ab52143c961b69216.
    let expected = json!({
        "version":1,
        "defaults":{"resources":{"cpus":2,"memory":4096}},
        "snapshot":{"include":[],"exclude":[],"allowSecrets":[],"maxFileSize":1},
        "tasks":{"check":{"command":"echo test","resources":{"cpus":1.5,"memory":512},"priority":-2,"shards":2,"retry":{"max":2,"on":["lost"]}}},
        "builds":{},"actions":{}
    });
    let raw: Value = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(raw["snapshot"]["maxFileSize"].as_u64().is_none());
    let normalized = normalize_project_config(raw).unwrap();
    assert_eq!(normalized, expected);
    // Consumers use integer accessors, so accepting a float without canonicalizing it
    // would silently lose max-file size, memory or shard settings later in the CLI.
    assert_eq!(normalized["snapshot"]["maxFileSize"].as_u64(), Some(1));
    assert_eq!(
        normalized["defaults"]["resources"]["memory"].as_u64(),
        Some(4096)
    );
    assert_eq!(normalized["tasks"]["check"]["shards"].as_u64(), Some(2));
    assert_eq!(
        normalized["tasks"]["check"]["resources"]["cpus"].as_f64(),
        Some(1.5)
    );
    let temporary = tempfile::tempdir().unwrap();
    write(temporary.path(), "gridstator.yml", yaml);
    assert_eq!(discover(temporary.path()).unwrap()["config"], expected);
    let maximum =
        normalize_project_config(json!({"snapshot":{"maxFileSize":9_007_199_254_740_991.0}}))
            .unwrap();
    assert_eq!(
        maximum["snapshot"]["maxFileSize"].as_u64(),
        Some(9_007_199_254_740_991)
    );
    for yaml in [
        "snapshot: { maxFileSize: 9007199254740992 }",
        "defaults: { resources: { memory: 9007199254740992.0 } }",
        "snapshot: { maxFileSize: 1.5 }",
        "defaults: { resources: { memory: 4096.5 } }",
        "tasks: { test: { command: echo, shards: 2.5 } }",
        "tasks: { test: { command: echo, priority: -2.5 } }",
        "tasks: { test: { command: echo, retry: { max: 2.5 } } }",
    ] {
        assert!(
            normalize_project_config(serde_yaml_ng::from_str(yaml).unwrap()).is_err(),
            "{yaml}"
        );
    }
}

#[test]
fn project_environment_limits_count_source_unicode_code_points() {
    let maximum = "🦀".repeat(128 * 1024);
    let accepted = normalize_project_config(
        json!({"tasks":{"test":{"command":"true","env":{"VALUE":maximum}}}}),
    );
    assert!(accepted.is_ok());
    assert!(
        normalize_project_config(
            json!({"tasks":{"test":{"command":"true","env":{"VALUE":"🦀".repeat(128*1024+1)}}}})
        )
        .is_err()
    );
}

#[test]
fn workflow_launcher_configuration_survives_discovery_and_preserves_defaults() {
    let defaults = normalize_project_config(json!({})).unwrap();
    assert!(defaults["actions"].get("launcher").is_none());
    let empty = normalize_project_config(json!({"actions":{"launcher":{}}})).unwrap();
    assert_eq!(empty["actions"]["launcher"], json!({}));
    let launcher = json!({
        "staging-local.yml":{"description":"Build locally","jobs":["local"]},
        ".github/workflows/coverage.yaml":{"jobs":["coverage","report"]},
        "all.yml":{}
    });
    let temporary = tempfile::tempdir().unwrap();
    write(
        temporary.path(),
        "gridstator.yml",
        &json!({"actions":{"launcher":launcher}}).to_string(),
    );
    let found = discover(temporary.path()).unwrap();
    assert_eq!(found["config"]["actions"]["launcher"], launcher);
}

#[test]
fn workflow_launcher_rejects_invalid_and_ambiguous_entries() {
    for launcher in [
        json!(null),
        json!([]),
        json!({"../internal.yml":{}}),
        json!({".github/workflows/../internal.yml":{}}),
        json!({".github/workflows/nested/internal.yml":{}}),
        json!({"sub/internal.yml":{}}),
        json!({"internal\\hidden.yml":{}}),
        json!({"internal.txt":{}}),
        json!({".yml":{}}),
        json!({"internal.yml":null}),
        json!({"internal.yml":{"unknown":true}}),
        json!({"internal.yml":{"description":1}}),
        json!({"internal.yml":{"jobs":[]}}),
        json!({"internal.yml":{"jobs":[""]}}),
        json!({"internal.yml":{"jobs":["  "]}}),
        json!({"internal.yml":{"jobs":[1]}}),
        json!({"internal.yml":{"jobs":["build","build"]}}),
        json!({"internal.yml":{},".github/workflows/internal.yml":{}}),
    ] {
        assert!(
            normalize_project_config(json!({"actions":{"launcher":launcher}})).is_err(),
            "{launcher}"
        );
    }
}
