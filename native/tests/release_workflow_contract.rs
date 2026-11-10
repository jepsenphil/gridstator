//! Source releases.test.ts001–007 with the approved native manifest/four-target artifact envelope.
//! Header fixtures exercise verification/activation state; they are never executed as candidates.
use axum::http::Method;
use axum::response::IntoResponse;
use gridstator::{
    controller::Controller,
    install::InstallStore,
    release::{
        ArtifactMirror, FileDigest, OfficialReleases, ProtocolCompatibility, RELEASE_MANIFEST_NAME,
        ReleaseArtifact, ReleaseManifest, SUPPORTED_TARGETS, host_target, sha256_bytes,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn current_payload() -> std::path::PathBuf {
    static PAYLOAD: OnceLock<(tempfile::TempDir, std::path::PathBuf)> = OnceLock::new();
    PAYLOAD
        .get_or_init(|| {
            let temp = tempfile::tempdir().unwrap();
            let binary = temp.path().join("release-test-payload");
            fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
            // Strip only fixture debug sections; embedded identity and executable behavior remain.
            // This avoids repeatedly copying/hashing hundreds of MiB of symbols in each child.
            #[cfg(target_os = "linux")]
            if !std::process::Command::new("strip")
                .arg("--strip-debug")
                .arg(&binary)
                .status()
                .is_ok_and(|status| status.success())
            {
                fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
            }
            (temp, binary)
        })
        .1
        .clone()
}

fn isolated(scenario: &str) {
    let temp = tempfile::tempdir().unwrap();
    let (current, files) = fixture(temp.path(), gridstator::BUILD_ID, gridstator::VERSION);
    let store = InstallStore::new(temp.path().join("install"));
    let artifact = current.target(host_target().unwrap()).unwrap();
    store
        .install_verified(
            &current,
            host_target().unwrap(),
            temp.path().join(&artifact.archive.file),
        )
        .unwrap();
    store.activate(gridstator::BUILD_ID, None).unwrap();
    drop(files);
    let output = std::process::Command::new(store.executable(gridstator::BUILD_ID).unwrap())
        .args(["--exact", "release_scenario_child", "--nocapture"])
        .env("GRIDSTATOR_RELEASE_TEST_SCENARIO", scenario)
        .env(
            "GRIDSTATOR_SUPERVISED",
            if scenario == "self003-unmanaged" {
                "0"
            } else {
                "1"
            },
        )
        .env("GRIDSTATOR_INSTALL_ROOT", temp.path().join("install"))
        .env("GRIDSTATOR_INSTALL_VERSION", gridstator::BUILD_ID)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "scenario{scenario}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn reference_controller_releases_001_cached_stable_and_prerelease_identity() {
    isolated("001");
}
#[test]
fn reference_controller_releases_002_verified_download_order_and_activation() {
    isolated("002");
}
#[test]
fn reference_controller_releases_003_corruption_cannot_change_active_install() {
    isolated("003");
}
#[test]
fn reference_controller_releases_004_bad_checksums_do_not_download_payloads() {
    isolated("004");
}
#[test]
fn reference_controller_releases_005_operator_lists_but_cannot_install() {
    isolated("005");
}
#[test]
fn reference_controller_releases_006_discovery_failure_is_not_cached_or_mutating() {
    isolated("006");
}
#[test]
fn reference_controller_releases_007_invalid_identifiers_never_leave_controller() {
    isolated("007");
}
#[test]
fn source_controller_self_update_003_refuses_unmanaged_and_bad_checksums() {
    isolated("self003-unmanaged");
    isolated("self003-checksum");
}
#[test]
fn same_active_build_id_cannot_rebind_the_controller_mirror() {
    isolated("same-id-collision");
}
#[test]
fn same_active_build_id_bootstrap_and_official_install_cannot_poison_mirror() {
    isolated("same-id-bootstrap");
}

fn header(target: &str) -> Vec<u8> {
    let mut bytes = vec![0; 128];
    if target.starts_with("linux/") {
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        let machine: u16 = if target.ends_with("amd64") { 62 } else { 183 };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    } else {
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        let machine: u32 = if target.ends_with("amd64") {
            0x0100_0007
        } else {
            0x0100_000c
        };
        bytes[4..8].copy_from_slice(&machine.to_le_bytes());
    }
    bytes
}
fn fixture(
    directory: &Path,
    id: &str,
    version: &str,
) -> (ReleaseManifest, BTreeMap<String, Vec<u8>>) {
    let mut files = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    for target in SUPPORTED_TARGETS {
        let current = id == gridstator::BUILD_ID;
        let bytes = if current && target == host_target().unwrap() {
            fs::read(current_payload()).unwrap()
        } else {
            header(target)
        };
        let name = format!(
            "gridstator-{version}-{}{}",
            target.replace('/', "-"),
            if current { "" } else { ".tar.gz" }
        );
        let archive = if current {
            bytes.clone()
        } else {
            let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            let mut record = tar::Header::new_gnu();
            record.set_size(bytes.len() as u64);
            record.set_mode(0o755);
            record.set_cksum();
            archive
                .append_data(&mut record, "gridstator", bytes.as_slice())
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap()
        };
        fs::write(directory.join(&name), &archive).unwrap();
        artifacts.insert(
            target.into(),
            ReleaseArtifact {
                archive: FileDigest {
                    file: name.clone(),
                    size: archive.len() as u64,
                    sha256: sha256_bytes(&archive),
                },
                executable: FileDigest {
                    file: "gridstator".into(),
                    size: bytes.len() as u64,
                    sha256: sha256_bytes(&bytes),
                },
            },
        );
        files.insert(name, archive);
    }
    (
        ReleaseManifest {
            schema_version: 1,
            build_id: id.into(),
            version: version.into(),
            source_revision: "fixture-only-never-executed".into(),
            protocol: ProtocolCompatibility::default(),
            artifacts,
        },
        files,
    )
}
struct Server {
    base: String,
    routes: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    fetched: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let fetched = Arc::new(Mutex::new(Vec::new()));
        let (routes_task, fetched_task) = (routes.clone(), fetched.clone());
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                fetched_task.lock().unwrap().push(path.clone());
                let body = routes_task.lock().unwrap().get(&path).cloned();
                let status = if body.is_some() { 200 } else { 404 };
                let body = body.unwrap_or_else(|| b"missing".to_vec());
                let _ = stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await;
                let _ = stream.write_all(&body).await;
            }
        });
        Self {
            base,
            routes,
            fetched,
            task,
        }
    }
    fn source(&self) -> OfficialReleases {
        OfficialReleases::with_base_urls(
            &format!("{}/api/", self.base),
            &format!("{}/download/", self.base),
            &format!("{}/web/", self.base),
        )
        .unwrap()
    }
    fn set(&self, path: &str, value: Vec<u8>) {
        self.routes.lock().unwrap().insert(path.into(), value);
    }
    fn paths(&self) -> Vec<String> {
        self.fetched.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.fetched.lock().unwrap().clear();
    }
}
async fn route(
    controller: &Arc<Controller>,
    method: Method,
    path: &str,
    query: HashMap<String, String>,
    role: &str,
) -> anyhow::Result<Value> {
    let response = gridstator::updates::route(
        controller.clone(),
        method,
        path,
        &query,
        json!({}),
        json!({"kind":"client","role":role}),
    )
    .await?
    .unwrap();
    Ok(
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1024 * 1024).await?)
            .unwrap(),
    )
}
async fn source_error_class(error: anyhow::Error, status: u16, code: &str) {
    let response = gridstator::controller::ApiError::from(error).into_response();
    assert_eq!(response.status().as_u16(), status);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["error"]["code"], code);
}
#[test]
fn release_scenario_child() {
    let Ok(scenario) = std::env::var("GRIDSTATOR_RELEASE_TEST_SCENARIO") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let store = InstallStore::new(std::env::var_os("GRIDSTATOR_INSTALL_ROOT").unwrap());
        let (old, _) = fixture(temp.path(), gridstator::BUILD_ID, gridstator::VERSION);
        store.install_verified(&old,host_target().unwrap(), temp.path().join(&old.target(host_target().unwrap()).unwrap().archive.file)).unwrap();
        store.activate(gridstator::BUILD_ID,None).unwrap();
        let (manifest, files) = fixture(temp.path(), "candidate-never-executed", "2.0.0");
        let server = Server::start().await;
        let manifest_bytes = manifest.to_bytes().unwrap();
        let mut checksums = format!("{}  {RELEASE_MANIFEST_NAME}\n",sha256_bytes(&manifest_bytes));
        let mut assets = vec![json!({"name":RELEASE_MANIFEST_NAME}),json!({"name":"SHA256SUMS"})];
        for (name,bytes) in files {checksums.push_str(&format!("{}  {name}\n",sha256_bytes(&bytes)));assets.push(json!({"name":name})); server.set(&format!("/download/v2.0.0/{name}"),bytes);}
        server.set(&format!("/download/v2.0.0/{RELEASE_MANIFEST_NAME}"),manifest_bytes);
        server.set("/download/v2.0.0/SHA256SUMS",checksums.clone().into_bytes());
        let stable = json!({"tag_name":"v2.0.0","name":"Gridstator v2.0.0","body":"A release with fixes.","published_at":"2026-10-01T00:00:00Z","draft":false,"prerelease":false,"assets":assets});
        server.set("/api/tags/v2.0.0",serde_json::to_vec(&stable).unwrap());
        let mut preview = stable.clone(); preview["tag_name"] = json!("v2.1.0-beta.1"); preview["prerelease"] = json!(true);
        let mut draft = stable.clone(); draft["draft"] = json!(true);
        let mut missing = stable.clone(); missing["assets"] = json!([]);
        let mut invalid = stable.clone(); invalid["tag_name"] = json!("not-a-version");
        server.set("/api/?per_page=100",serde_json::to_vec(&[stable.clone(),preview,draft,missing,invalid]).unwrap());
        let directory = temp.path().join("controller");
        gridstator::controller::initialize(&directory,"127.0.0.1",7300,&[],false,&temp.path().join("client.json")).await.unwrap();
        let controller = Controller::open_with_releases(directory.clone(),server.source()).await.unwrap();
        match scenario.as_str() {
            "001" => {
                let (first,second) = tokio::join!(route(&controller,Method::GET,"/v1/releases",HashMap::new(),"operator"),route(&controller,Method::GET,"/v1/releases",HashMap::from([("prerelease".into(),"true".into())]),"operator"));
                let stable = first.unwrap();let all = second.unwrap();
                assert_eq!(stable["releases"].as_array().unwrap().iter().map(|value|value["tag"].as_str().unwrap()).collect::<Vec<_>>(),["v2.0.0"]);
                assert_eq!(all["releases"].as_array().unwrap().iter().map(|value|value["tag"].as_str().unwrap()).collect::<Vec<_>>(),["v2.0.0","v2.1.0-beta.1"]);
                assert_eq!(stable["releases"][0]["packageName"],RELEASE_MANIFEST_NAME);
                assert_eq!(stable["releases"][0]["manifestName"],RELEASE_MANIFEST_NAME);
                assert_eq!(stable["releases"][0]["url"],format!("{}/web/v2.0.0",server.base));
                assert_eq!(server.paths(),["/api/?per_page=100"]);
                let view = route(&controller,Method::GET,"/v1/updates",HashMap::new(),"operator").await.unwrap();
                assert_eq!(view["controller"]["selfUpdate"],json!({"available":true,"reason":null}));
            }
            "002" => {
                let response = route(&controller,Method::POST,"/v1/releases/v2.0.0/install",HashMap::new(),"admin").await.unwrap();
                assert_eq!(response,json!({"action":"restarting","from":gridstator::BUILD_ID,"to":manifest.build_id}));
                let mut expected = vec!["/api/tags/v2.0.0".into(),"/download/v2.0.0/SHA256SUMS".into(),format!("/download/v2.0.0/{RELEASE_MANIFEST_NAME}")];
                for target in SUPPORTED_TARGETS {expected.push(format!("/download/v2.0.0/{}",manifest.target(target).unwrap().archive.file));}
                assert_eq!(server.paths(),expected);
                assert_eq!(store.state().unwrap().unwrap().current,gridstator::BUILD_ID,"native controller drains before activation");
                let mirror = ArtifactMirror::new(directory.join("native-release-mirror"));
                let bytes = old.to_bytes().unwrap();mirror.cache_manifest(&bytes,&sha256_bytes(&bytes)).unwrap();
                for target in SUPPORTED_TARGETS {mirror.cache_artifact(&old,target,temp.path().join(&old.target(target).unwrap().archive.file)).unwrap();}
                gridstator::updates::tick(controller.clone()).await.unwrap();
                let state = store.state().unwrap().unwrap();assert_eq!(state.current,manifest.build_id);assert_eq!(state.pending.unwrap().previous,gridstator::BUILD_ID);
                assert!(directory.join("updates/controller-switch.json").is_file());
                assert_eq!(server.paths(),expected,"activation uses the verified mirror without downloading again");
            }
            "003" => {
                let artifact = manifest.target(SUPPORTED_TARGETS[0]).unwrap();let mut corrupt = fs::read(temp.path().join(&artifact.archive.file)).unwrap();corrupt[0]^=1;server.set(&format!("/download/v2.0.0/{}",artifact.archive.file),corrupt);
                let error=route(&controller,Method::POST,"/v1/releases/v2.0.0/install",HashMap::new(),"admin").await.unwrap_err();assert!(error.to_string().contains("sha256 mismatch: expected"),"{error:#}");source_error_class(error,400,"BAD_REQUEST").await;
                assert!(!store.root().join("versions").join(&manifest.build_id).exists());
                assert!(controller.db.call(|connection|gridstator::db::get(connection,"updates","controller")).await.unwrap().is_none());
            }
            "004" => {
                for body in [format!("{}  other\n","a".repeat(64)),format!("{}  {RELEASE_MANIFEST_NAME}\n{}  {RELEASE_MANIFEST_NAME}\n","a".repeat(64),"b".repeat(64))] {
                    server.clear();server.set("/download/v2.0.0/SHA256SUMS",body.into_bytes());
                    let error=route(&controller,Method::POST,"/v1/releases/v2.0.0/install",HashMap::new(),"admin").await.unwrap_err();assert!(error.to_string().contains("published checksums"));source_error_class(error,400,"BAD_REQUEST").await;
                    assert_eq!(server.paths(),["/api/tags/v2.0.0","/download/v2.0.0/SHA256SUMS"]);
                }
            }
            "005" => {
                assert_eq!(route(&controller,Method::GET,"/v1/releases",HashMap::new(),"operator").await.unwrap()["releases"].as_array().unwrap().len(),1);server.clear();
                let error=route(&controller,Method::POST,"/v1/releases/v2.0.0/install",HashMap::new(),"operator").await.unwrap_err();assert!(error.to_string().starts_with("FORBIDDEN:"));source_error_class(error,403,"FORBIDDEN").await;assert!(server.paths().is_empty());
            }
            "006" => {
                server.routes.lock().unwrap().remove("/api/?per_page=100");
                let error=route(&controller,Method::GET,"/v1/releases",HashMap::new(),"operator").await.unwrap_err();assert!(error.to_string().starts_with("UNAVAILABLE: Could not check Gridstator releases"));source_error_class(error,503,"UNAVAILABLE").await;
                server.set("/api/?per_page=100",serde_json::to_vec(&[stable]).unwrap());
                assert_eq!(route(&controller,Method::GET,"/v1/releases",HashMap::new(),"operator").await.unwrap()["releases"].as_array().unwrap().len(),1);assert_eq!(server.paths().len(),2,"failed discovery is not cached");
            }
            "007" => {
                for tag in ["https://example.com/package","..","v2.0.0/extra","arbitrary"] {let error=route(&controller,Method::POST,&format!("/v1/releases/{tag}/install"),HashMap::new(),"admin").await.unwrap_err();assert!(error.to_string().starts_with("BAD_REQUEST: Choose a published Gridstator release tag."));source_error_class(error,400,"BAD_REQUEST").await;}
                assert!(server.paths().is_empty());
            }
            "self003-unmanaged"|"self003-checksum" => {
                let bundle=temp.path().join("refused-bundle.tar.gz");fs::write(&bundle,b"untrusted bytes").unwrap();
                let error=gridstator::updates::publish_path(controller.clone(),&bundle,&"0".repeat(64),json!({"kind":"client","role":"admin"})).await.unwrap_err();
                if scenario=="self003-unmanaged" {assert!(error.to_string().contains("not a managed install"),"{error:#}");source_error_class(error,409,"CONFLICT").await;}
                else {assert!(error.to_string().contains("expected 0000"),"{error:#}");source_error_class(error,400,"BAD_REQUEST").await;}
                assert!(controller.db.call(|connection|gridstator::db::get(connection,"updates","controller")).await.unwrap().is_none());
            }
            "same-id-collision" => {
                let pack=|manifest:&ReleaseManifest,files:&BTreeMap<String,Vec<u8>>| {
                    let encoder=flate2::write::GzEncoder::new(Vec::new(),flate2::Compression::fast());let mut archive=tar::Builder::new(encoder);
                    let bytes=manifest.to_bytes().unwrap();let mut members=files.clone();members.insert(RELEASE_MANIFEST_NAME.into(),bytes);
                    for (name,bytes) in members {let mut header=tar::Header::new_gnu();header.set_size(bytes.len() as u64);header.set_mode(0o600);header.set_cksum();archive.append_data(&mut header,name,bytes.as_slice()).unwrap();}
                    archive.into_inner().unwrap().finish().unwrap()
                };
                let (mut collision,files)=fixture(temp.path(),"collision",gridstator::VERSION);collision.build_id=gridstator::BUILD_ID.into();
                let bytes=pack(&collision,&files);let bundle=temp.path().join("same-id-collision.tar.gz");fs::write(&bundle,&bytes).unwrap();
                let error=gridstator::updates::publish_path(controller.clone(),&bundle,&sha256_bytes(&bytes),json!({"kind":"client","role":"admin"})).await.unwrap_err();
                assert!(error.to_string().contains("immutable active native build"),"{error:#}");
                let mirror=ArtifactMirror::new(directory.join("native-release-mirror"));assert!(mirror.manifest(gridstator::BUILD_ID).is_err(),"rejected collision must not populate the mirror");
                assert_eq!(store.manifest(gridstator::BUILD_ID).unwrap(),old);
                // Honest repeated publication remains idempotent.
                let files=old.artifacts.values().map(|artifact|(artifact.archive.file.clone(),fs::read(temp.path().join(&artifact.archive.file)).unwrap())).collect();
                let bytes=pack(&old,&files);fs::write(&bundle,&bytes).unwrap();
                for _ in 0..2 {let response=gridstator::updates::publish_path(controller.clone(),&bundle,&sha256_bytes(&bytes),json!({"kind":"client","role":"admin"})).await.unwrap();let value:Value=serde_json::from_slice(&axum::body::to_bytes(response.into_body(),1024*1024).await.unwrap()).unwrap();assert_eq!(value["action"],"none");assert_eq!(mirror.manifest(gridstator::BUILD_ID).unwrap(),old);}
            }
            "same-id-bootstrap" => {
                let (mut collision,files)=fixture(temp.path(),"collision",gridstator::VERSION);collision.build_id=gridstator::BUILD_ID.into();
                let tag=format!("v{}",gridstator::VERSION);let bytes=collision.to_bytes().unwrap();
                let mut checksums=format!("{}  {RELEASE_MANIFEST_NAME}\n",sha256_bytes(&bytes));
                let mut assets=vec![json!({"name":RELEASE_MANIFEST_NAME}),json!({"name":"SHA256SUMS"})];
                for (name,bytes) in files {checksums.push_str(&format!("{}  {name}\n",sha256_bytes(&bytes)));assets.push(json!({"name":name}));}
                server.set(&format!("/api/tags/{tag}"),serde_json::to_vec(&json!({"tag_name":tag,"name":"Collision","body":"","published_at":"2026-10-01T00:00:00Z","draft":false,"prerelease":false,"assets":assets})).unwrap());
                server.set(&format!("/download/{tag}/SHA256SUMS"),checksums.into_bytes());server.set(&format!("/download/{tag}/{RELEASE_MANIFEST_NAME}"),bytes);
                gridstator::updates::tick(controller.clone()).await.unwrap();
                let preparation=controller.db.call(|connection|gridstator::db::get(connection,"updates","mirrorPreparation")).await.unwrap().unwrap();assert_eq!(preparation["phase"],"failed");assert!(preparation["error"].as_str().unwrap().contains("immutable active native build"));
                let expected=[format!("/api/tags/{tag}"),format!("/download/{tag}/SHA256SUMS"),format!("/download/{tag}/{RELEASE_MANIFEST_NAME}")];assert_eq!(server.paths(),expected);
                let mirror=ArtifactMirror::new(directory.join("native-release-mirror"));assert!(mirror.manifest(gridstator::BUILD_ID).is_err());
                server.clear();let error=route(&controller,Method::POST,&format!("/v1/releases/{tag}/install"),HashMap::new(),"admin").await.unwrap_err();assert!(error.to_string().contains("immutable active native build"));source_error_class(error,400,"BAD_REQUEST").await;assert_eq!(server.paths(),expected);assert!(mirror.manifest(gridstator::BUILD_ID).is_err());assert_eq!(store.manifest(gridstator::BUILD_ID).unwrap(),old);
            }
            _ => panic!("unknown scenario"),
        }
        if scenario != "002" {let state=store.state().unwrap().unwrap();assert_eq!(state.current,gridstator::BUILD_ID);assert!(state.pending.is_none());}
    });
}
