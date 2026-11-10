use gridstator::executor::{self, ActLogParser, ExecutionContext, ExecutionResult};
use gridstator::workspace::{self, BlobStore, CaptureOptions};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn fixture(spec: Value, source: Option<&Path>) -> (TempDir, ExecutionContext) {
    let temp = TempDir::new().unwrap();
    let store = BlobStore::new(temp.path().join("cas"));
    store.init().unwrap();
    let manifest = source.map(|source| {
        let capture = workspace::capture(source, CaptureOptions::default()).unwrap();
        for (digest, source) in &capture.blobs {
            store
                .put_bytes(digest, &source.read_verified(digest).unwrap())
                .unwrap();
        }
        capture.manifest
    });
    let id = ulid::Ulid::new().to_string();
    let context = ExecutionContext {
        job_id: id.clone(),
        assignment_id: ulid::Ulid::new().to_string(),
        attempt: 1,
        node_id: format!("native-executor-contract-{id}"),
        node_name: "executor-contract".into(),
        spec,
        work_dir: temp.path().join("work").join(id),
        manifest,
        blobs: store,
        keep_failed: false,
        act_bin: None,
    };
    (temp, context)
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn require_tool(tool: &str, args: &[&str]) {
    let output = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("required {tool} unavailable: {error}"));
    assert!(
        output.status.success(),
        "required {tool} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn command_spec(command: &str) -> Value {
    json!({"kind":"command","image":"alpine:3.20","command":["sh","-c",command],"resources":{"cpus":1,"memoryMb":128},"env":{},"secretEnv":{},"workdir":".","artifacts":[]})
}

async fn run(context: ExecutionContext) -> (ExecutionResult, Vec<(String, String)>) {
    let (send, mut receive) = mpsc::channel(128);
    let collector = tokio::spawn(async move {
        let mut logs = Vec::new();
        while let Some(entry) = receive.recv().await {
            logs.push(entry);
        }
        logs
    });
    let result = executor::execute(context, CancellationToken::new(), send)
        .await
        .unwrap();
    (result, collector.await.unwrap())
}

fn assert_clean(context: &ExecutionContext) {
    let filter = format!("label=io.gridstator.job={}", context.job_id);
    for resource in ["container", "volume"] {
        let mut command = Command::new("docker");
        command.args([resource, "ls", "-q", "--filter", &filter]);
        if resource == "container" {
            command.arg("-a");
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert!(
            output.stdout.is_empty(),
            "orphan {resource}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn environment_plan_keeps_multiline_secrets_out_of_argument_values() {
    let (_temp, context) = fixture(command_spec("true"), None);
    let spec = json!({"env":{"PLAIN":"ordinary"},"secretEnv":{"TOKEN":"first\nsecond"}});
    let plan = executor::plan_environment(&spec, &context).unwrap();
    assert!(plan.file_content.contains("PLAIN=ordinary\n"));
    assert!(!plan.file_content.contains("first"));
    assert_eq!(plan.pass_through["TOKEN"], "first\nsecond");
    assert!(
        executor::plan_environment(&json!({"env":{"DOCKER_HOST":"one\ntwo"}}), &context).is_err()
    );
}

#[test]
fn checkout_rewrite_changes_only_same_repository_and_preserves_foreign_refs() {
    let source = "name: Test\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n        with: {ref: '${{ github.sha }}', repository: acme/demo, fetch-depth: 0}\n      - uses: actions/checkout@v4\n        with: {ref: main, repository: other/project}\n";
    let (text, rewrites) = executor::rewrite_local_checkouts(source, Some("acme/demo")).unwrap();
    assert_eq!(rewrites.len(), 1);
    assert!(!text.contains("github.sha"));
    assert!(text.contains("other/project"));
    assert!(text.contains("ref: main"));
    assert!(text.contains("fetch-depth: 0"));
    let invalid = "jobs: [";
    assert_eq!(
        executor::rewrite_local_checkouts(invalid, None).unwrap().0,
        invalid
    );
}

#[test]
fn act_log_parser_tracks_steps_formats_output_and_clamps_negative_duration() {
    let mut parser = ActLogParser::default();
    let started = parser.feed(&json!({"job":"CI/test","stage":"Main","stepID":["0"],"step":"Test","msg":"⭐ Run Main Test","level":"info"}).to_string(), "stderr");
    assert!(started[0].1.contains("[CI/test] ⭐ Run"));
    let output = parser.feed(
        &json!({"job":"CI/test","msg":"hello\nworld","raw_output":true}).to_string(),
        "stderr",
    );
    assert_eq!(output[0], ("stdout".into(), "[CI/test]   | hello\n".into()));
    parser.feed(&json!({"job":"CI/test","stage":"Main","stepID":["0"],"step":"Test","stepResult":"success","executionTime":-1}).to_string(), "stderr");
    assert_eq!(parser.steps()[0]["status"], "success");
    assert_eq!(parser.steps()[0]["durationMs"], 0);
    assert_eq!(
        parser.feed("non-json", "stderr"),
        [("stderr".into(), "non-json\n".into())]
    );
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn real_docker_command_stages_snapshot_reports_exit_artifacts_and_cleans() {
    require_tool("docker", &["version"]);
    let source = TempDir::new().unwrap();
    write(source.path(), "src/value", "uncommitted");
    let mut spec = command_spec(
        "pwd; printf 'utf8: 💾\\n'; cat value; printf '\\n'; printf '%s' \"$TOKEN\"; printf '\\n'; mkdir -p out; echo report > out/result; exit 7",
    );
    spec["workdir"] = json!("src");
    spec["secretEnv"] = json!({"TOKEN":"line1\nline2"});
    spec["artifacts"] = json!(["src/out"]);
    let (_temp, context) = fixture(spec, Some(source.path()));
    let (result, logs) = run(context.clone()).await;
    assert_eq!(result.outcome, "failed");
    assert_eq!(result.exit_code, Some(7));
    assert!(!result.oom_killed);
    let stdout = logs
        .iter()
        .filter(|(stream, _)| stream == "stdout")
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    assert!(stdout.starts_with("/workspace/src\nutf8: 💾\n"), "{stdout}");
    assert!(stdout.contains("uncommitted"));
    assert!(stdout.contains("line1\nline2"));
    assert!(
        logs.iter()
            .any(|(stream, text)| stream == "phase" && text == "running")
    );
    assert_eq!(result.artifacts.len(), 1);
    assert_eq!(result.artifacts[0].name, "src/out.tar");
    let mut archive = tar::Archive::new(std::fs::File::open(&result.artifacts[0].path).unwrap());
    let mut found = false;
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().as_ref() == Path::new("out/result") {
            let mut contents = String::new();
            std::io::Read::read_to_string(&mut entry, &mut contents).unwrap();
            assert_eq!(contents, "report\n");
            found = true;
        }
    }
    assert!(
        found,
        "Docker cp artifact contains the requested file and bytes"
    );
    assert_clean(&context);
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn real_docker_cancellation_and_timeout_stop_actual_container() {
    require_tool("docker", &["version"]);
    let (_temp, context) = fixture(command_spec("echo started; exec sleep 60"), None);
    let token = CancellationToken::new();
    let cancel = token.clone();
    let (send, mut receive) = mpsc::channel(128);
    let task = tokio::spawn(executor::execute(context.clone(), token, send));
    let started = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some((_, text)) = receive.recv().await {
            if text.contains("started") {
                return;
            }
        }
        panic!("job exited before running");
    })
    .await;
    assert!(started.is_ok(), "container did not start");
    cancel.cancel();
    let drain = tokio::spawn(async move { while receive.recv().await.is_some() {} });
    let result = tokio::time::timeout(Duration::from_secs(35), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drain.await.unwrap();
    assert_eq!(result.outcome, "cancelled");
    assert_clean(&context);
    let mut spec = command_spec("exec sleep 60");
    spec["timeoutSeconds"] = json!(1);
    let (_temp, context) = fixture(spec, None);
    let (result, _) = run(context.clone()).await;
    assert_eq!(result.outcome, "failed");
    assert_eq!(result.error.as_deref(), Some("Timed out after 1s"));
    assert_clean(&context);
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn real_buildx_uses_stage_secret_outputs_and_removes_temporary_export_tag() {
    require_tool("docker", &["buildx", "version"]);
    let source = TempDir::new().unwrap();
    write(
        source.path(),
        "Dockerfile",
        "FROM alpine:3.20 AS base\nARG GREETING=hello\nRUN --mount=type=secret,id=TOKEN test \"$(cat /run/secrets/TOKEN)\" = private-value && echo \"$GREETING\" > /result\nFROM base AS final\nLABEL native.contract=yes\n",
    );
    let spec = json!({"kind":"docker-build","context":".","stage":"final","buildArgs":{"GREETING":"native"},"secretEnv":{"TOKEN":"private-value"},"output":"tar","tags":[],"platforms":[],"cacheFrom":[],"cacheTo":[]});
    let (_temp, context) = fixture(spec, Some(source.path()));
    let (result, logs) = run(context.clone()).await;
    assert_eq!(
        result.outcome,
        "succeeded",
        "{}\n{}",
        result.error.as_deref().unwrap_or(""),
        logs.iter()
            .map(|(_, text)| text.as_str())
            .collect::<String>()
    );
    assert_eq!(result.outputs["image"]["output"], "tar");
    assert!(
        result.outputs["image"]["id"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(result.artifacts[0].name, "image.tar");
    assert!(std::fs::metadata(&result.artifacts[0].path).unwrap().len() > 1000);
    assert!(result.outputs["image"]["sizeBytes"].as_u64().unwrap() > 0);
    let tag = format!("gridstator-export/{}:latest", context.job_id.to_lowercase());
    assert!(
        !Command::new("docker")
            .args(["image", "inspect", &tag])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn real_act_sees_exact_snapshot_private_checkout_copies_inputs_and_steps() {
    require_tool("act", &["--version"]);
    require_tool("docker", &["version"]);
    require_tool("git", &["--version"]);
    let source = TempDir::new().unwrap();
    write(source.path(), "value.txt", "from uncommitted snapshot\n");
    write(
        source.path(),
        ".github/workflows/ci.yml",
        "name: Native\non:\n  workflow_dispatch:\n    inputs:\n      who: {description: Who, required: false}\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n        with: { ref: '${{ github.sha }}' }\n      - name: Snapshot\n        run: cat value.txt\n      - name: Original workflow\n        run: |\n          grep -q 'with: { ref:' .github/workflows/ci.yml && echo original-workflow\n      - name: Context\n        run: echo \"repo=$GITHUB_REPOSITORY ref=$GITHUB_REF_NAME input=${{ inputs.who }}\"\n      - name: Secrets\n        run: |\n          echo \"single=${{ secrets.SINGLE }}\"\n          printf '%s' \"${{ secrets.MULTI }}\" | wc -l\n      - run: mkdir -p out && cp value.txt out/value.txt\n      - uses: actions/upload-artifact@v4\n        with: {name: results, path: out/}\n",
    );
    let spec = json!({"kind":"actions","workflow":".github/workflows/ci.yml","event":"workflow_dispatch","inputs":{"who":"native"},"secretEnv":{"SINGLE":"single-secret-value","MULTI":"l1\nl2\nl3"},"platforms":{"ubuntu-latest":"node:24-bookworm-slim"},"git":{"branch":"feature/native","remoteUrl":"https://github.com/acme/demo.git"}});
    let (_temp, context) = fixture(spec, Some(source.path()));
    let (result, logs) = run(context.clone()).await;
    let output = logs
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    assert_eq!(
        result.outcome,
        "succeeded",
        "{}\n{output}",
        result.error.as_deref().unwrap_or("")
    );
    assert!(output.contains("from uncommitted snapshot"));
    assert!(output.contains("original-workflow"));
    assert!(
        output.contains("repo=acme/demo ref=feature/native input=native"),
        "{output}"
    );
    assert!(
        result.outputs["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["name"] == "Snapshot" && step["status"] == "success")
    );
    assert!(logs.iter().any(|(stream, _)| stream == "outputs"));
    assert!(output.contains("single=***"), "{output}");
    assert!(!output.contains("single-secret-value"), "{output}");
    assert!(output.lines().any(|line| line.ends_with("| 2")), "{output}");
    let steps = result.outputs["steps"].as_array().unwrap();
    for name in ["actions/checkout@v4", "Snapshot", "Secrets"] {
        assert!(steps.iter().any(|step| step["name"] == name));
    }
    assert!(
        steps
            .iter()
            .all(|step| step["status"] == "success" || step["status"] == "skipped")
    );
    assert_eq!(
        result
            .artifacts
            .iter()
            .map(|artifact| artifact.name.as_str())
            .collect::<Vec<_>>(),
        ["results/results.zip"]
    );
    assert!(std::fs::metadata(&result.artifacts[0].path).unwrap().len() > 0);
    assert_clean(&context);
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn real_act_local_reusable_workflow_keeps_snapshot_and_collects_artifact() {
    require_tool("act", &["--version"]);
    require_tool("docker", &["version"]);
    let source = TempDir::new().unwrap();
    write(source.path(), "value.txt", "called-workflow-snapshot\n");
    write(
        source.path(),
        ".github/workflows/main.yml",
        "name: Caller\non: push\njobs:\n  calls:\n    uses: ./.github/workflows/called.yml\n",
    );
    write(
        source.path(),
        ".github/workflows/called.yml",
        "name: Called\non: workflow_call\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n        with: {ref: '${{ github.sha }}'}\n      - name: Read snapshot\n        run: cat value.txt\n      - run: mkdir -p out && cp value.txt out/value.txt\n      - uses: actions/upload-artifact@v4\n        with: {name: native-results, path: out/}\n",
    );
    let spec = json!({"kind":"actions","workflow":".github/workflows/main.yml","event":"push","platforms":{"ubuntu-latest":"node:24-bookworm-slim"},"git":{"branch":"feature/called","remoteUrl":"https://github.com/acme/demo.git"}});
    let (_temp, context) = fixture(spec, Some(source.path()));
    let (result, logs) = run(context.clone()).await;
    let output = logs
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    assert_eq!(
        result.outcome,
        "succeeded",
        "{}\n{output}",
        result.error.as_deref().unwrap_or("")
    );
    assert!(output.contains("called-workflow-snapshot"), "{output}");
    assert!(
        logs.iter().any(|(stream, text)| stream == "system"
            && text.contains(".github/workflows/called.yml: job test")),
        "{logs:?}"
    );
    assert_eq!(
        result
            .artifacts
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["native-results/native-results.zip"]
    );
    assert!(std::fs::metadata(&result.artifacts[0].path).unwrap().len() > 0);
    assert_clean(&context);
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn real_act_cancellation_cleans_labelled_workflow_containers() {
    require_tool("act", &["--version"]);
    require_tool("docker", &["version"]);
    let source = TempDir::new().unwrap();
    write(
        source.path(),
        ".github/workflows/slow.yml",
        "name: Slow\non: push\njobs:\n  wait:\n    runs-on: ubuntu-latest\n    steps:\n      - name: Wait\n        run: mkdir -p cancellation-created; echo cleanup > cancellation-created/file.txt; echo native-cancel-started; sleep 60\n",
    );
    let spec = json!({"kind":"actions","workflow":".github/workflows/slow.yml","event":"push","dockerSocket":true,"platforms":{"ubuntu-latest":"node:24-bookworm-slim"}});
    let (_temp, context) = fixture(spec, Some(source.path()));
    let token = CancellationToken::new();
    let cancel = token.clone();
    let (send, mut receive) = mpsc::channel(128);
    let task = tokio::spawn(executor::execute(context.clone(), token, send));
    tokio::time::timeout(Duration::from_secs(45), async {
        while let Some((_, text)) = receive.recv().await {
            if text.contains("| native-cancel-started") {
                return;
            }
        }
        panic!("act exited before its workflow started");
    })
    .await
    .unwrap();
    cancel.cancel();
    let drain = tokio::spawn(async move { while receive.recv().await.is_some() {} });
    let result = tokio::time::timeout(Duration::from_secs(40), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drain.await.unwrap();
    assert_eq!(result.outcome, "cancelled");
    assert!(
        result.outputs["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["name"] == "Wait" && step["status"] == "cancelled")
    );
    assert_clean(&context);
    std::fs::remove_dir_all(&context.work_dir).expect("cancelled workflow workdir is removable");
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn real_act_socket_opt_in_shares_runner_files_with_nested_container() {
    require_tool("act", &["--version"]);
    require_tool("docker", &["version"]);
    let source = TempDir::new().unwrap();
    write(
        source.path(),
        ".github/workflows/nested.yml",
        r#"name: Nested
on: push
jobs:
  nested:
    runs-on: ubuntu-latest
    steps:
      - name: Prepare shared file
        run: mkdir -p exchange && echo from-runner > exchange/input.txt
      - name: Run nested container
        run: |
          node --input-type=module <<'NODE'
          import http from 'node:http';
          const request = (method, path, body) => new Promise((resolve, reject) => {
            const req = http.request({socketPath:'/var/run/docker.sock', method, path, headers:{'content-type':'application/json'}}, res => {
              let data=''; res.setEncoding('utf8'); res.on('data', c => data+=c);
              res.on('end', () => res.statusCode >= 300 ? reject(new Error(data)) : resolve(data ? JSON.parse(data) : {}));
            });
            req.on('error', reject); req.end(body === undefined ? undefined : JSON.stringify(body));
          });
          const child = await request('POST', '/containers/create', {
            Image:'alpine:3.20',
            Labels:{'io.gridstator.managed':'true','io.gridstator.node':process.env.TEST_NODE,'io.gridstator.job':process.env.TEST_JOB},
            Cmd:['sh','-c','test "$(cat /workspace/exchange/input.txt)" = from-runner && mkdir -p /workspace/root-created && echo from-child > /workspace/root-created/output.txt'],
            HostConfig:{Binds:[process.cwd()+':/workspace']},
          });
          try {
            await request('POST', '/containers/'+child.Id+'/start');
            const result = await request('POST', '/containers/'+child.Id+'/wait');
            if(result.StatusCode !== 0) throw new Error('nested child failed');
          } finally { await request('DELETE', '/containers/'+child.Id+'?force=true'); }
          NODE
      - name: Read shared result
        run: cat root-created/output.txt
"#,
    );
    let spec = json!({"kind":"actions","workflow":".github/workflows/nested.yml","event":"push","dockerSocket":true,"platforms":{"ubuntu-latest":"node:24-bookworm-slim"}});
    let (_temp, mut context) = fixture(spec, Some(source.path()));
    context.spec["env"] = json!({"TEST_NODE":context.node_id,"TEST_JOB":context.job_id});
    let (result, logs) = run(context.clone()).await;
    let output = logs
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    assert_eq!(
        result.outcome,
        "succeeded",
        "{}\n{output}",
        result.error.as_deref().unwrap_or("")
    );
    assert!(output.contains("| from-child"), "{output}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(context.work_dir.join("src/root-created/output.txt"))
                .unwrap()
                .uid(),
            std::fs::metadata(context.work_dir.join("src"))
                .unwrap()
                .uid()
        );
    }
    assert_clean(&context);
    std::fs::remove_dir_all(&context.work_dir)
        .expect("nested-container workflow workdir is removable");
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn cleanup_respects_owned_node_and_keeps_explicit_active_jobs() {
    require_tool("docker", &["version"]);
    let (_temp, context) = fixture(command_spec("true"), None);
    let name = format!("native-contract-owned-{}", context.job_id.to_lowercase());
    let output = Command::new("docker")
        .args([
            "create",
            "--name",
            &name,
            "--label",
            "io.gridstator.managed=true",
            "--label",
            &format!("io.gridstator.node={}", context.node_id),
            "--label",
            &format!("io.gridstator.job={}", context.job_id),
            "alpine:3.20",
            "true",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        executor::cleanup_managed("a-different-test-node", &HashSet::new())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        executor::cleanup_managed(&context.node_id, &HashSet::from([context.job_id.clone()]))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        executor::cleanup_managed(&context.node_id, &HashSet::new())
            .await
            .unwrap(),
        1
    );
    assert_clean(&context);
}

#[tokio::test]
#[ignore = "requires real Docker/buildx/act; run the explicit tool suite in TESTING.md"]
async fn keep_failed_preserves_command_resources_until_explicit_owned_cleanup() {
    require_tool("docker", &["version"]);
    let (_temp, mut context) = fixture(command_spec("exit 3"), None);
    context.keep_failed = true;
    let (result, _) = run(context.clone()).await;
    assert_eq!(result.outcome, "failed");
    assert_eq!(result.exit_code, Some(3));
    let output = Command::new("docker")
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=io.gridstator.job={}", context.job_id),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        !output.stdout.is_empty(),
        "failed container must be retained for inspection"
    );
    assert_eq!(
        executor::cleanup_managed(&context.node_id, &HashSet::new())
            .await
            .unwrap(),
        2
    );
    assert_clean(&context);
}

#[tokio::test]
#[ignore = "requires real Docker/buildx; run the explicit tool suite in TESTING.md"]
async fn real_buildx_load_describes_image_and_preserves_stage_args_and_labels() {
    require_tool("docker", &["buildx", "version"]);
    let source = TempDir::new().unwrap();
    write(
        source.path(),
        "Dockerfile",
        "FROM alpine:3.20 AS base\nARG APP_VERSION=unset\nRUN --mount=type=secret,id=TOKEN test \"$(sha256sum /run/secrets/TOKEN | cut -d ' ' -f1)\" = 4218638739c44bf8204ae61b4a63975c645657439fdb979325b4c26b090467db && echo build-secret-present && echo \"$APP_VERSION\" > /version\nFROM base AS runner\nLABEL native.contract=yes\n",
    );
    let tag = format!(
        "gridstator-contract-load:{}",
        ulid::Ulid::new().to_string().to_lowercase()
    );
    let spec = json!({"kind":"docker-build","stage":"runner","tags":[tag],"buildArgs":{"APP_VERSION":"9.9.9"},"secretEnv":{"TOKEN":"private-value"},"output":"load"});
    let (_temp, context) = fixture(spec, Some(source.path()));
    let (result, logs) = run(context.clone()).await;
    let text = logs
        .iter()
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    let inspect = Command::new("docker")
        .args(["image", "inspect", &tag])
        .output()
        .unwrap();
    let reported_inspect = result.outputs["image"]["id"].as_str().map(|id| {
        Command::new("docker")
            .args(["image", "inspect", id])
            .output()
            .unwrap()
    });
    let version = Command::new("docker")
        .args(["run", "--rm", "--entrypoint", "cat", &tag, "/version"])
        .output()
        .unwrap();
    let cleanup = Command::new("docker")
        .args(["image", "rm", &tag])
        .output()
        .unwrap();
    assert_eq!(result.outcome, "succeeded", "{:?}\n{text}", result.error);
    assert!(result.error.is_none());
    assert_eq!(result.outputs["image"]["tags"], json!([tag]));
    assert_eq!(result.outputs["image"]["output"], "load");
    assert!(
        result.outputs["image"]["id"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(result.outputs["image"]["sizeBytes"].as_u64().unwrap() > 0);
    assert!(text.contains("build-secret-present"));
    assert!(!text.contains("private-value"));
    assert!(inspect.status.success());
    let image: Value = serde_json::from_slice(&inspect.stdout).unwrap();
    let reported_inspect = reported_inspect.expect("local output must report an image reference");
    assert!(
        reported_inspect.status.success(),
        "reported image reference is not addressable: {}",
        String::from_utf8_lossy(&reported_inspect.stderr)
    );
    let reported: Value = serde_json::from_slice(&reported_inspect.stdout).unwrap();
    assert_eq!(reported[0]["Id"], image[0]["Id"]);
    assert_eq!(result.outputs["image"]["sizeBytes"], image[0]["Size"]);
    assert_eq!(reported[0]["Size"], image[0]["Size"]);
    assert_eq!(
        image[0]["Config"]["Labels"]["io.gridstator.job"],
        context.job_id
    );
    assert_eq!(
        image[0]["Config"]["Labels"]["io.gridstator.node"],
        context.node_id
    );
    assert!(version.status.success());
    assert_eq!(String::from_utf8(version.stdout).unwrap(), "9.9.9\n");
    assert!(cleanup.status.success());
}

#[tokio::test]
#[ignore = "requires real Docker/buildx; run the explicit tool suite in TESTING.md"]
async fn real_buildx_failure_and_missing_dockerfile_return_terminal_results() {
    require_tool("docker", &["buildx", "version"]);
    let source = TempDir::new().unwrap();
    write(
        source.path(),
        "Dockerfile",
        "FROM alpine:3.20\nRUN echo native-build-boom && false\n",
    );
    let (_temp, context) = fixture(
        json!({"kind":"docker-build","output":"none"}),
        Some(source.path()),
    );
    let (result, _) = run(context).await;
    assert_eq!(result.outcome, "failed");
    assert_eq!(result.exit_code, Some(1));
    let (_temp, context) = fixture(
        json!({"kind":"docker-build","dockerfile":"nope/Dockerfile"}),
        Some(source.path()),
    );
    let (result, _) = run(context).await;
    assert_eq!(result.outcome, "failed");
    assert!(
        result
            .error
            .unwrap()
            .contains("Dockerfile \"nope/Dockerfile\" does not exist")
    );
}

#[tokio::test]
#[ignore = "requires real Docker/buildx; run the explicit tool suite in TESTING.md"]
async fn real_buildx_cancellation_interrupts_running_build() {
    require_tool("docker", &["buildx", "version"]);
    let source = TempDir::new().unwrap();
    write(
        source.path(),
        "Dockerfile",
        &format!(
            "FROM alpine:3.20\nRUN echo native-build-started-{} && sleep 120\n",
            ulid::Ulid::new()
        ),
    );
    let (_temp, context) = fixture(
        json!({"kind":"docker-build","output":"none","noCache":true}),
        Some(source.path()),
    );
    let token = CancellationToken::new();
    let cancel = token.clone();
    let (send, mut receive) = mpsc::channel(128);
    let task = tokio::spawn(executor::execute(context, token, send));
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some((_, text)) = receive.recv().await {
            if text.contains("native-build-started-") {
                return;
            }
        }
        panic!("build ended before running");
    })
    .await
    .expect("running build marker");
    let start = std::time::Instant::now();
    cancel.cancel();
    let drain = tokio::spawn(async move { while receive.recv().await.is_some() {} });
    let result = tokio::time::timeout(Duration::from_secs(25), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drain.await.unwrap();
    assert_eq!(result.outcome, "cancelled");
    assert!(start.elapsed() < Duration::from_secs(25));
}

#[tokio::test]
#[ignore = "requires real Docker/act; run the explicit tool suite in TESTING.md"]
async fn real_act_failed_job_and_missing_workflow_preserve_source_errors() {
    require_tool("act", &["--version"]);
    require_tool("docker", &["version"]);
    let source = TempDir::new().unwrap();
    write(
        source.path(),
        ".github/workflows/test.yml",
        "name: Test\non: push\njobs:\n  fails:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo about-to-fail && exit 3\n",
    );
    let spec = json!({"kind":"actions","workflow":".github/workflows/test.yml","job":"fails","platforms":{"ubuntu-latest":"node:24-bookworm-slim"}});
    let (_temp, context) = fixture(spec, Some(source.path()));
    let (result, logs) = run(context.clone()).await;
    assert_eq!(result.outcome, "failed", "{:?}\n{logs:?}", result.error);
    assert_eq!(
        result.error.as_deref(),
        Some("Workflow job(s) failed: Test/fails")
    );
    assert!(
        result.outputs["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["status"] == "failure")
    );
    assert_clean(&context);
    let (_temp, context) = fixture(
        json!({"kind":"actions","workflow":".github/workflows/missing.yml"}),
        Some(source.path()),
    );
    let (result, _) = run(context).await;
    assert_eq!(result.outcome, "failed");
    assert!(result.error.unwrap().contains("does not exist"));
}

#[tokio::test]
#[ignore = "requires real Docker; run the explicit tool suite in TESTING.md"]
async fn real_docker_success_streams_env_cache_and_missing_image_is_clear() {
    require_tool("docker", &["version"]);
    let mut spec = command_spec(
        "echo out; echo err >&2; echo \"$GRIDSTATOR_JOB_ID\"; echo \"$PLAIN\"; printf '%s' \"$MULTI\" | wc -l; echo \"$npm_config_cache\"",
    );
    spec["env"] = json!({"PLAIN":"hello world"});
    spec["secretEnv"] = json!({"MULTI":"line1\nline2\nline3"});
    let (_temp, context) = fixture(spec, None);
    let (result, logs) = run(context.clone()).await;
    assert_eq!(result.outcome, "succeeded");
    assert_eq!(result.exit_code, Some(0));
    assert!(result.error.is_none());
    assert!(!result.oom_killed);
    let stdout = logs
        .iter()
        .filter(|(stream, _)| stream == "stdout")
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    let stderr = logs
        .iter()
        .filter(|(stream, _)| stream == "stderr")
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    for expected in [
        "out\n",
        &context.job_id,
        "hello world",
        "2\n",
        "/gridstator/cache/npm",
    ] {
        assert!(stdout.contains(expected), "{stdout}");
    }
    assert!(stderr.contains("err\n"));
    assert!(
        logs.iter()
            .any(|(stream, text)| stream == "phase" && text == "running")
    );
    assert_clean(&context);
    let mut spec = command_spec("true");
    spec["image"] = json!("gridstator-does-not-exist.invalid/nope:1");
    let (_temp, context) = fixture(spec, None);
    let (result, _) = run(context.clone()).await;
    assert_eq!(result.outcome, "failed");
    assert!(result.error.unwrap().contains("Could not create container"));
    assert_clean(&context);
}
