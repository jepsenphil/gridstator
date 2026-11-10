#!/usr/bin/env python3
"""Opt-in actual ARM Linux workflow, orchestrated outside an unprivileged Docker CLI container.

Run on an authorized Apple Silicon validation host. Inputs are explicit; the script owns only its
unique outer container/state volume/tag and native job labels. Python is a test orchestrator on
macOS, never a required worker runtime. No privileged container, Desktop restart or systemd claim.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import signal
import ssl
import subprocess
import tarfile
import time
import urllib.error
import urllib.parse
import urllib.request

REDACT = re.compile(r"gs[acj]_[A-Za-z0-9_-]+")


def scrub(text):
    return REDACT.sub("<redacted>", text)


class Proof:
    def __init__(self, args):
        self.args = args
        self.root = Path(args.root).resolve()
        assert self.root.name.startswith("linux-arm-vm-"), "explicit private fixture namespace required"
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.container = args.name
        assert self.container.startswith("gridstator-linux-arm-proof-"), "unique owned name required"
        self.volume = self.container + "-state"
        self.tag = "gridstator-linux-arm-proof:" + args.name.removeprefix("gridstator-linux-arm-proof-")
        self.env = dict(os.environ)
        self.env.update(PATH="/usr/bin:/bin:/usr/sbin:/sbin", DOCKER_CONFIG=str(self.root / "docker-config"), DOCKER_HOST=args.docker_socket)
        (self.root / "docker-config").mkdir(mode=0o700, exist_ok=True)
        (self.root / "docker-config/config.json").write_text(json.dumps({"cliPluginsExtraDirs": ["/Applications/Docker.app/Contents/Resources/cli-plugins"]}))
        self.steps = []
        self.node_ids = []
        self.started = False
        self.volume_created = False
        self.report = {"result": "running", "scope": "Actual static Linux ARM64 native CLI/controller/embedded+standalone workers inside unprivileged M4 Docker ARM Linux VM; external Docker/buildx/git/act only", "expectedBuildId": args.build_id, "expectedExecutableSha256": args.sha256, "actors": [], "scenarios": {}}

    def save(self, name, value):
        (self.root / name).write_text(json.dumps(value, indent=2) + "\n")

    def docker(self, *args, data=None, check=True, timeout=120):
        result = subprocess.run([self.args.docker, *args], env=self.env, input=data, text=True, capture_output=True, timeout=timeout)
        if check and result.returncode:
            raise AssertionError(scrub(f"Docker {args[:3]} failed {result.returncode}: {result.stderr[-4000:]}"))
        return result

    def inner(self, *args, data=None, check=True, timeout=120, cwd=None):
        opts = ["exec"]
        if data is not None:
            opts += ["-i"]
        if cwd:
            opts += ["-w", cwd]
        return self.docker(*opts, self.container, *args, data=data, check=check, timeout=timeout)

    def cli(self, *args, cwd="/proof/project", timeout=120):
        started = time.time()
        p = self.inner("/input/gridstator", *args, cwd=cwd, timeout=timeout)
        self.steps.append({"argv": ["gridstator", *args], "cwd": cwd, "exitCode": p.returncode, "seconds": round(time.time()-started, 3), "stdout": scrub(p.stdout), "stderr": scrub(p.stderr)})
        self.save("commands.json", self.steps)
        return p

    def api(self, path, body=None, method=None, token=None):
        payload = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.url + path, data=payload, method=method or ("POST" if payload is not None else "GET"), headers={"authorization": "Bearer " + (token or self.config["token"]), "content-type": "application/json"})
        with urllib.request.urlopen(request, context=self.context, timeout=10) as response:
            return json.load(response)

    def wait(self, function, label, seconds=120):
        deadline = time.monotonic() + seconds
        last = None
        while time.monotonic() < deadline:
            try:
                value = function()
                if value:
                    return value
            except (OSError, urllib.error.URLError, KeyError) as error:
                last = str(error)
            time.sleep(0.15)
        raise AssertionError(f"{label} deadline: {scrub(last or '')}")

    def start_controller(self, embedded):
        args = " --with-agent --agent-name arm-embedded --max-concurrency 1 --reserve-cpus 0 --reserve-memory 0" if embedded else ""
        command = "echo $$ > /proof/controller.pid; exec /input/gridstator controller start --data-dir /proof/controller" + args + " >> /proof/controller.log 2>&1"
        self.docker("exec", "-d", self.container, "sh", "-c", command)
        self.wait(lambda: self.api("/v1/health"), "native TLS controller startup")

    def stop_role(self, name):
        self.inner("sh", "-c", f"test ! -f /proof/{name}.pid || kill -TERM $(cat /proof/{name}.pid)", check=False)
        self.wait(lambda: self.inner("sh", "-c", f"test ! -f /proof/{name}.pid || ! kill -0 $(cat /proof/{name}.pid) 2>/dev/null", check=False).returncode == 0, name + " shutdown", 30)

    def nodes(self):
        return self.api("/v1/nodes")["nodes"]

    def online(self):
        nodes = self.nodes()
        selected = [n for n in nodes if n["name"] in ["arm-embedded", "arm-standalone"] and n["status"] == "online" and n["info"]["docker"]["available"] and n["info"]["act"]["available"]]
        return selected if len(selected) == 2 else None

    def submit(self, *args):
        options = list(args)
        options.insert(options.index("--") if "--" in options else len(options), "--json")
        result = json.loads(self.cli(*options).stdout)
        assert result.get("id"), result
        return result

    def job(self, submitted, expected="succeeded", seconds=180):
        job_id = submitted["id"]
        def done():
            j = self.api("/v1/jobs/" + job_id)
            if j["state"] in ["succeeded", "failed", "cancelled", "lost"]:
                assert j["state"] == expected, scrub(json.dumps(j))
                return j
            return None
        final = self.wait(done, "job " + job_id, seconds)
        logs = self.api("/v1/jobs/" + job_id + "/logs")
        return final, logs

    def stdout(self, logs):
        return "".join(e["d"] for e in logs["entries"] if e["o"] == "stdout")

    def scenario(self, name, value):
        self.report["scenarios"][name] = value
        self.save("proof.json", self.report)
        print(("DIAGNOSTIC " if value.get("strictImageIdentityPassed") is False else "PASSED ") + name, flush=True)

    def run(self):
        binary = Path(self.args.gridstator)
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == self.args.sha256
        assert self.docker("container", "inspect", self.container, check=False).returncode != 0, "refusing existing container"
        assert self.docker("volume", "inspect", self.volume, check=False).returncode != 0, "refusing existing state volume"
        image = json.loads(self.docker("image", "inspect", self.args.image).stdout)[0]
        assert image["Architecture"] == "arm64" and image["Os"] == "linux"
        self.save("official-image.json", image)
        self.docker("volume", "create", "--label", "io.gridstator.validation=" + self.container, self.volume)
        self.volume_created = True
        self.docker("run", "-d", "--name", self.container, "--label", "io.gridstator.validation=" + self.container, "--platform", "linux/arm64", "--cpus", "2", "--memory", "1g", "--pids-limit", "512", "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--read-only", "--tmpfs", "/tmp:rw,size=128m", "--publish", "127.0.0.1::4242", "--mount", "type=volume,source=" + self.volume + ",target=/proof", "--mount", "type=bind,source=" + str(binary.parent) + ",target=/input,readonly", "--mount", "type=bind,source=" + self.args.docker_socket.removeprefix("unix://") + ",target=/var/run/docker.sock", "-e", "DOCKER_HOST=unix:///var/run/docker.sock", "-e", "DOCKER_CONFIG=/proof/docker-config", "-e", "DOCKER_TLS_CERTDIR=", "-e", "HOME=/proof/home", "-e", "GRIDSTATOR_CONFIG=/proof/client.json", "-e", "GRIDSTATOR_CACHE=/proof/cache", "-e", "GRIDSTATOR_AGENT_DATA=/proof/embedded", "-e", "GRIDSTATOR_ACT=/input/act", self.args.image, "sleep", "3600")
        self.started = True
        inspected = json.loads(self.docker("inspect", self.container).stdout)[0]
        assert not inspected["HostConfig"]["Privileged"]
        self.url = "https://127.0.0.1:" + inspected["NetworkSettings"]["Ports"]["4242/tcp"][0]["HostPort"]
        self.save("outer-container.json", {k: inspected[k] for k in ["Id", "Image", "Platform", "Path", "Args", "HostConfig", "NetworkSettings"]})
        self.inner("sh", "-c", "umask 077; mkdir -p /proof/project /proof/docker-config /proof/home; for tool in node npm cargo rustc python python3; do if command -v \"$tool\"; then exit 91; fi; done; command -v docker; command -v git; uname -a; docker version --format '{{json .}}'; docker buildx version; git --version; /input/act --version")
        tools = self.inner("sh", "-c", "uname -a; printf 'PATH=%s\\n' \"$PATH\"; docker version --format '{{json .}}'; docker buildx version; git --version; /input/act --version").stdout
        self.save("toolchain.json", {"output": tools, "workerPathAbsent": ["node", "npm", "cargo", "rustc", "python", "python3"], "pythonOrchestrator": "macOS host only"})
        info = json.loads(self.inner("/input/gridstator", "build-info", "--json").stdout)
        assert info["buildId"] == self.args.build_id and info["sha256"] == self.args.sha256 and info["target"] == "linux/arm64"
        self.report["actualBuildInfo"] = info
        init = self.inner("/input/gridstator", "controller", "init", "--data-dir", "/proof/controller", "--host", "0.0.0.0", "--port", "4242", "--san", "127.0.0.1", "localhost")
        self.save("initialization.json", {"exitCode": init.returncode, "stderr": scrub(init.stderr)})
        self.config = json.loads(self.inner("cat", "/proof/client.json").stdout)
        self.config["url"] = "https://127.0.0.1:4242"
        self.inner("sh", "-c", "umask 077; cat > /proof/client.json", data=json.dumps(self.config))
        self.context = ssl.create_default_context(cadata=self.config["caPem"])
        self.start_controller(False)
        self.api("/v1/settings", {"autoUpdate": "off", "toolchain": {"act": "off"}}, "PATCH")
        self.stop_role("controller")
        self.start_controller(True)
        self.wait(lambda: next((n for n in self.nodes() if n["name"] == "arm-embedded" and n["status"] == "online" and n["info"]["act"]["available"]), None), "embedded readiness")
        embedded = next(n for n in self.nodes() if n["name"] == "arm-embedded")
        join_token = self.api("/v1/tokens", {"kind": "join", "name": "private-arm-vm", "maxUses": 1})["token"]
        registered = self.api("/v1/agents/register", {"name": "arm-standalone", "info": embedded["info"], "settings": {"autoUpdate": "off", "maxConcurrency": 1, "reserved": {"cpus": 0, "memoryMb": 0}}}, token=join_token)
        worker = {"version": 2, "controllerUrl": self.config["url"], "caPem": self.config["caPem"], "caFingerprint": self.config["caFingerprint"], "nodeId": registered["nodeId"], "name": registered["name"], "credential": registered["credential"], "settings": {"autoUpdate": "off", "maxConcurrency": 1, "reserved": {"cpus": 0, "memoryMb": 0}}}
        self.inner("sh", "-c", "umask 077; mkdir -p /proof/standalone; cat > /proof/standalone/agent.json", data=json.dumps(worker))
        self.docker("exec", "-d", self.container, "sh", "-c", "echo $$ > /proof/standalone.pid; exec /input/gridstator agent start --data-dir /proof/standalone >> /proof/standalone.log 2>&1")
        actors = self.wait(self.online, "two actual ARM workers")
        self.report["actors"] = actors
        self.node_ids = [n["id"] for n in actors]
        for n in actors:
            assert n["info"]["platform"] == "linux" and n["info"]["arch"] == "arm64"
            b = n["info"]["build"]
            assert b["id"] == self.args.build_id and b["target"] == "linux/arm64" and b["runtime"] == "rust" and b["sha256"] == self.args.sha256 and "node" not in b
            assert b["tools"]["act"] == "0.2.96" and "0.2.96" in n["info"]["act"]["version"]
        self.node_ids = [n["id"] for n in actors]
        self.report["actors"] = actors
        project = self.root / "project"
        (project / ".github/workflows").mkdir(parents=True, exist_ok=True)
        (project / "proof.txt").write_text("arm-snapshot\n")
        (project / ".env").write_text("PRIVATE_SHOULD_NOT_UPLOAD=not-a-real-secret\n")
        (project / "Dockerfile").write_text("FROM alpine:3.21\nCOPY proof.txt /proof.txt\nRUN test \"$(uname -m)\" = aarch64 && test \"$(cat /proof.txt)\" = arm-snapshot\n")
        (project / "gridstator.yml").write_text('''version: 1
project: linux-arm-vm-proof
defaults:
  image: alpine:3.21
  resources:
    cpus: 1
    memory: 128m
actions:
  platforms:
    ubuntu-latest: alpine:3.21
tasks:
  shards:
    command: >-
      mkdir -p reports &&
      printf '<testsuite tests="1" failures="0"><testcase classname="arm" name="shard-{shard}"/></testsuite>' > reports/junit.xml &&
      echo shard-{shard}/{total} && sleep 2
    shards: 2
    junit: reports/junit.xml
''')
        (project / ".github/workflows/arm.yml").write_text('''name: ARM
on: [workflow_dispatch]
jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - name: Native ARM shell
        shell: sh
        run: |
          uname -m
          test "$(uname -m)" = aarch64
          echo arm-act-verified
''')
        self.docker("cp", str(project) + "/.", self.container + ":/proof/project")
        submitted = self.submit("run", "--node", "arm-embedded", "--image", "alpine:3.21", "--cpus", "1", "--memory", "128m", "--artifact", "results", "--", "sh", "-c", 'set -eu; test "$(uname -m)" = aarch64; test "$(cat proof.txt)" = arm-snapshot; test ! -e .env; mkdir -p results; printf "aarch64 artifact\\n" > results/proof.txt; cat results/proof.txt; echo embedded-arm')
        j, logs = self.job(submitted)
        assert self.stdout(logs) == "aarch64 artifact\nembedded-arm\n" and j["nodeName"] == "arm-embedded"
        artifacts = json.loads(self.cli("artifacts", j["id"], "--json").stdout)
        artifact = next(a for a in artifacts if a["name"] == "results.tar")
        self.cli("artifacts", j["id"], "--download", artifact["name"], "--out", "/proof/result.tar")
        self.docker("cp", self.container + ":/proof/result.tar", str(self.root / "result.tar"))
        raw = (self.root / "result.tar").read_bytes()
        assert "sha256:" + hashlib.sha256(raw).hexdigest() == artifact["digest"]
        with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
            member = next(m for m in archive.getmembers() if m.name.endswith("proof.txt"))
            assert archive.extractfile(member).read() == b"aarch64 artifact\n"
        snapshot = self.api("/v1/snapshots/" + urllib.parse.quote(j["snapshotId"], safe="") + "?manifest=true")
        manifest = snapshot.get("manifest", snapshot)
        assert "proof.txt" in [e["path"] for e in manifest["entries"]] and ".env" not in [e["path"] for e in manifest["entries"]]
        self.scenario("embedded-command-snapshot-artifact", {"job": j, "logs": logs, "artifact": artifact, "snapshot": snapshot, "downloadSha256": hashlib.sha256(raw).hexdigest()})
        submitted = self.submit("build", "--node", "arm-standalone", "--cpus", "1", "--memory", "128m", "--tag", self.tag, "--output", "load")
        built, logs = self.job(submitted, seconds=240)
        image = json.loads(self.docker("image", "inspect", self.tag).stdout)[0]
        by_id = self.docker("image", "inspect", str(built["outputs"]["image"]["id"]), check=False)
        self.save("build-before-assert.json", {"job": built, "logs": logs, "image": image, "inspectReportedId": {"exitCode": by_id.returncode, "stdout": by_id.stdout, "stderr": by_id.stderr}})
        assert built["nodeName"] == "arm-standalone"
        resolved = json.loads(by_id.stdout) if by_id.returncode == 0 else []
        identity_matches = len(resolved) == 1 and resolved[0]["Id"] == image["Id"] and built["outputs"]["image"]["sizeBytes"] == image["Size"]
        if not identity_matches:
            repro_tag = self.tag + "-metadata-repro"
            try:
                direct = self.inner("env", "BUILDX_NO_DEFAULT_ATTESTATIONS=1", "docker", "buildx", "build", "--progress=plain", "--metadata-file", "/proof/direct-metadata.json", "--load", "--tag", repro_tag, "/proof/project", timeout=120)
                raw_metadata = json.loads(self.inner("cat", "/proof/direct-metadata.json").stdout)
                direct_image = json.loads(self.docker("image", "inspect", repro_tag).stdout)[0]
                config_probe = self.docker("image", "inspect", str(raw_metadata.get("containerimage.config.digest")), check=False)
                self.save("direct-buildkit-metadata-repro.json", {"command": ["env", "BUILDX_NO_DEFAULT_ATTESTATIONS=1", "docker", "buildx", "build", "--progress=plain", "--metadata-file", "/proof/direct-metadata.json", "--load", "--tag", repro_tag, "/proof/project"], "metadata": raw_metadata, "image": direct_image, "inspectConfigId": {"exitCode": config_probe.returncode, "stdout": config_probe.stdout, "stderr": config_probe.stderr}, "stdout": direct.stdout, "stderr": direct.stderr, "scope": "Independent same-tool native ARM BuildKit metadata witness; native job diagnostic is build-before-assert.json"})
            finally:
                self.docker("image", "rm", repro_tag, check=False)
            assert self.args.continue_known_build_id_mismatch, "reported build image ID is not the actual engine ID"
            assert built["outputs"]["image"]["digest"] == image["Id"] and by_id.returncode != 0, "unexpected image identity defect; diagnostic bypass does not apply"
            self.report["knownBuildIdMismatch"] = {"reportedId": built["outputs"]["image"]["id"], "actualId": image["Id"], "reportedSizeBytes": built["outputs"]["image"]["sizeBytes"], "actualSizeBytes": image["Size"], "strictAssertionFailed": True}
        assert image["Architecture"] == "arm64" and image["Os"] == "linux" and image["Config"]["Labels"]["io.gridstator.job"] == built["id"]
        run = self.docker("run", "--rm", "--network", "none", self.tag, "sh", "-c", "uname -m; cat /proof.txt")
        assert run.stdout == "aarch64\narm-snapshot\n"
        self.scenario("standalone-build-load-image", {"job": built, "logs": logs, "imageId": image["Id"], "architecture": image["Architecture"], "imageRunStdout": run.stdout, "strictImageIdentityPassed": identity_matches})
        workflows = json.loads(self.cli("actions", "list", "--json").stdout)
        action = self.submit("actions", "run", "arm.yml", "--job", "verify", "--node", "arm-standalone", "--cpus", "1", "--memory", "128m")
        ran, logs = self.job(action, seconds=180)
        steps = ran["outputs"]["steps"]
        assert any(s["name"] == "Native ARM shell" and s["status"] == "success" for s in steps)
        assert "aarch64" in self.stdout(logs) and "arm-act-verified" in self.stdout(logs)
        self.scenario("actual-verified-arm-act-shell", {"job": ran, "logs": logs, "workflows": workflows, "actOverride": "/input/act", "dockerSocketInRunner": False, "scope": "Real shell step; not GitHub JS actions or runner artifact-service coverage"})
        group = self.submit("run", "--task", "shards")
        self.wait(lambda: self.api("/v1/groups/" + group["id"])["state"] == "succeeded", "two-worker shard group")
        shown = json.loads(self.cli("group", "show", group["id"], "--json").stdout)
        g = shown["group"]
        assert set(g["nodes"]) == {"arm-embedded", "arm-standalone"}
        assert shown["tests"]["total"] == 2 and shown["tests"]["passed"] == 2 and shown["tests"]["failed"] == 0
        jobs = g["jobs"]
        assert max(j["startedAt"] for j in jobs) < min(j["finishedAt"] for j in jobs)
        self.scenario("two-worker-shards-combined-junit", {"group": g, "tests": shown["tests"], "overlapped": True})
        cancel = self.submit("run", "--node", "arm-standalone", "--image", "alpine:3.21", "--cpus", "1", "--memory", "128m", "--no-snapshot", "--", "sh", "-c", "echo arm-cancel-ready; sleep 30")
        self.wait(lambda: any("arm-cancel-ready" in e["d"] for e in self.api("/v1/jobs/" + cancel["id"] + "/logs")["entries"]), "actual cancel container")
        self.cli("cancel", cancel["id"])
        cancelled, logs = self.job(cancel, expected="cancelled")
        assert not self.docker("ps", "-aq", "--filter", "label=io.gridstator.job=" + cancel["id"]).stdout.strip()
        self.scenario("standalone-cancel-owned-cleanup", {"job": cancelled, "logs": logs, "ownedContainerAbsent": True})
        doctor = self.cli("doctor")
        assert "2/2 online" in doctor.stdout and "✓ controller" in doctor.stdout
        self.scenario("doctor-two-native-workers", {"stdout": doctor.stdout, "stderr": scrub(doctor.stderr), "exitCode": doctor.returncode})
        self.report["result"] = "diagnostic-only" if self.report.get("knownBuildIdMismatch") else "passed"
        self.report["limits"] = ["ARM Linux VM kernel and actual ARM64 payloads/tools, not a physical Linux ARM host or x86 emulation claim.", "Foreground native roles inside one unprivileged Docker CLI container; no systemd, service boot/reboot or OS installation claim.", "macOS Python3 only orchestrates tests; workerPATH contains no Node/npm/Rust/Cargo/Python.", "Docker socket is an explicitly authorized external engine; no privileged container or Desktop/global configuration change.", "Actions proves an actual verified external ARM act shell step; JavaScript actions and Actions runner artifact-service are outside this bounded scenario.", "Unsigned uncommitted preview; no stable release/signing/publication provenance claim."]
        self.save("proof.json", self.report)

    def cleanup(self):
        audit = {"ownedOuterContainer": self.container, "ownedStateVolume": self.volume, "nodeIds": self.node_ids, "existingDesktopRestarted": False, "privilegedContainerUsed": False, "globalDockerConfigurationChanged": False}
        if self.started:
            for role in ["standalone", "controller"]:
                try:
                    self.stop_role(role)
                except Exception as error:
                    audit[role + "StopError"] = scrub(str(error))
            for name in ["controller.log", "standalone.log"]:
                self.docker("cp", self.container + ":/proof/" + name, str(self.root / name), check=False)
            self.docker("rm", "-f", self.container)
            self.started = False
        resources = []
        for node in self.node_ids:
            for kind, cmd in [("containers", ["ps", "-aq"]), ("volumes", ["volume", "ls", "-q"])]:
                ids = self.docker(*cmd, "--filter", "label=io.gridstator.node=" + node).stdout.split()
                resources.append({"nodeId": node, "kind": kind, "remainingIds": ids})
                assert not ids, (kind, ids)
        audit["ownedJobResources"] = resources
        self.docker("image", "rm", self.tag, check=False)
        if self.volume_created:
            self.docker("volume", "rm", self.volume)
            self.volume_created = False
        audit.update(ownedOuterContainerRemoved=True, privateStateVolumeAndCredentialsRemoved=True, ownedImageTagRemoved=True)
        self.save("cleanup.json", audit)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--gridstator", required=True)
    parser.add_argument("--act", required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--build-id", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--docker", default="/Applications/Docker.app/Contents/Resources/bin/docker")
    parser.add_argument("--docker-socket", required=True)
    parser.add_argument("--continue-known-build-id-mismatch", action="store_true", help="Diagnostic only: continue after config-ID/engine-manifest-ID mismatch; overall result remains diagnostic and exit3")
    args = parser.parse_args()
    assert Path(args.act).resolve().parent == Path(args.gridstator).resolve().parent, "read-only tool input directory must contain both binaries"
    proof = Proof(args)
    started = time.time()
    try:
        proof.run()
    except Exception as error:
        proof.report.update(result="failed", error=scrub(str(error)))
        proof.save("proof.json", proof.report)
        raise
    finally:
        proof.report["seconds"] = round(time.time()-started, 3)
        proof.save("proof.json", proof.report)
        proof.cleanup()
    if proof.report["result"] == "diagnostic-only":
        print("Diagnostic ARM workflow completed; known build identity assertion remains failed; owned resources and credentials removed", flush=True)
        raise SystemExit(3)
    print("Actual Linux ARM workflow passed; owned resources and credentials removed", flush=True)


if __name__ == "__main__":
    main()
