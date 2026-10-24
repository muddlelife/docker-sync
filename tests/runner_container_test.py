"""Linux/Docker 发布验收：真实 Skopeo、临时 TLS 仓库，无需云端凭据。"""
import argparse
import base64
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

from cryptography.hazmat.primitives.ciphers.aead import AESGCM


def run(*args, env=None, check=True):
    return subprocess.run(args, env=env, capture_output=True, text=True, check=check)


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def main(image):
    registry_name = "docker-sync-smoke-" + uuid.uuid4().hex[:12]
    runner_uid, runner_gid = os.getuid() or 10001, os.getgid() or 10001
    with tempfile.TemporaryDirectory(prefix="docker-sync-container-") as tmp:
        root = Path(tmp)
        root.chmod(0o755)
        certificate, private_key = root / "ca.crt", root / "server.key"
        run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            "-keyout", str(private_key), "-out", str(certificate), "-subj", "/CN=localhost",
            "-addext", "subjectAltName=DNS:localhost")
        # 固定的非生产测试凭据，bcrypt 哈希由 htpasswd -nbB 生成。
        (root / "htpasswd").write_text("smoke:$2y$05$368BZhy0YolBTrkhlRBE0O8M4AzhAYRsG.3UqLEArsMGZ.pPpcN5u\n")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        endpoint = f"localhost:{port}"
        base = "https://" + endpoint
        context = ssl.create_default_context(cafile=str(certificate))
        client = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))

        def request(path, method="GET", data=None, content_type=None):
            url = path if path.startswith("https://") else base + path
            headers = {"Accept": "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json"}
            headers["Authorization"] = "Basic " + base64.b64encode(b"smoke:fixture-private-password").decode()
            if content_type:
                headers["Content-Type"] = content_type
            try:
                return client.open(urllib.request.Request(url, data=data, headers=headers, method=method), timeout=10)
            except urllib.error.HTTPError as error:
                detail = error.read(2048).decode("utf-8", "replace")
                raise RuntimeError(f"{method} {urllib.parse.urlsplit(url).path}: {detail}") from error

        try:
            run("docker", "run", "-d", "--rm", "--name", registry_name,
                "-p", f"127.0.0.1:{port}:5000",
                "--mount", f"type=bind,src={root},dst=/certs,readonly",
                "-e", "REGISTRY_AUTH=htpasswd", "-e", "REGISTRY_AUTH_HTPASSWD_REALM=smoke",
                "-e", "REGISTRY_AUTH_HTPASSWD_PATH=/certs/htpasswd",
                "-e", "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/ca.crt",
                "-e", "REGISTRY_HTTP_TLS_KEY=/certs/server.key", "registry:3")
            for _ in range(60):
                try:
                    with request("/v2/") as response:
                        if response.status == 200:
                            break
                except Exception:
                    time.sleep(.5)
            else:
                raise RuntimeError("临时 TLS 仓库未就绪")

            try:
                client.open(base + "/v2/", timeout=10)
            except urllib.error.HTTPError as error:
                assert error.code == 401
            else:
                raise AssertionError("私有仓库必须拒绝匿名访问")

            def upload_blob(data):
                with request("/v2/source/demo/blobs/uploads/", "POST", b"") as response:
                    location = urllib.parse.urljoin(base, response.headers["Location"])
                location += ("&" if "?" in location else "?") + urllib.parse.urlencode({"digest": digest(data)})
                with request(location, "PUT", data, "application/octet-stream") as response:
                    assert response.status == 201
                return digest(data)

            archive = io.BytesIO()
            with tarfile.open(fileobj=archive, mode="w") as tar:
                data = b"Skopeo transfer verification\n"
                entry = tarfile.TarInfo("verification.txt")
                entry.size = len(data)
                tar.addfile(entry, io.BytesIO(data))
            layer = gzip.compress(archive.getvalue(), mtime=0)
            layer_digest = upload_blob(layer)
            descriptors, leaves = [], {}
            for arch in ["amd64", "arm64"]:
                config = json.dumps({"architecture": arch, "os": "linux", "config": {},
                                     "rootfs": {"type": "layers", "diff_ids": [digest(archive.getvalue())]}}).encode()
                config_digest = upload_blob(config)
                media = "application/vnd.oci.image.manifest.v1+json"
                manifest = json.dumps({"schemaVersion": 2, "mediaType": media,
                                       "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "size": len(config), "digest": config_digest},
                                       "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "size": len(layer), "digest": layer_digest}]}).encode()
                with request(f"/v2/source/demo/manifests/{arch}", "PUT", manifest, media) as response:
                    assert response.status == 201
                leaves[arch] = digest(manifest)
                descriptors.append({"mediaType": media, "size": len(manifest), "digest": leaves[arch], "platform": {"os": "linux", "architecture": arch}})
            index_media = "application/vnd.oci.image.index.v1+json"
            index = json.dumps({"schemaVersion": 2, "mediaType": index_media, "manifests": descriptors}).encode()
            with request("/v2/source/demo/manifests/latest", "PUT", index, index_media) as response:
                assert response.status == 201

            def execute(name, key=None, target_password="fixture-private-password"):
                task_id = str(uuid.uuid4())
                items = [{"item_id": str(uuid.uuid4()), "source": endpoint + "/source/demo:latest",
                          "target": endpoint + "/target/demo:" + label, "platform": platform,
                          "source_credentials": {"username": "smoke", "password": "fixture-private-password"},
                          "target_credentials": {"username": "smoke", "password": target_password}}
                         for label, platform in [("all", "all"), ("amd64", "linux/amd64"), ("arm64", "linux/arm64")]]
                plain = json.dumps({"version": 1, "task_id": task_id, "items": items}).encode()
                secret = bytes([2]) * 32
                nonce = os.urandom(12)
                encrypted = nonce + AESGCM(secret).encrypt(nonce, plain, f"docker-sync:task:v1:{task_id}".encode())
                env = {**os.environ, "TASK_ID": task_id, "TASK_PROTOCOL_VERSION": "1",
                       "TASK_PAYLOAD": base64.urlsafe_b64encode(encrypted).decode().rstrip("="),
                       "SYNC_PAYLOAD_KEY": secret.hex() if key is None else key, "GITHUB_RUN_ATTEMPT": "3", "GITHUB_ACTIONS": "true"}
                output_dir = root / name
                output_dir.mkdir()
                if os.getuid() == 0:
                    os.chown(output_dir, runner_uid, runner_gid)
                command = ["docker", "run", "--rm", "--network", "host", "--user", f"{runner_uid}:{runner_gid}",
                           "--mount", f"type=bind,src={certificate},dst=/etc/containers/certs.d/{endpoint}/ca.crt,readonly",
                           "--mount", f"type=bind,src={output_dir},dst=/artifacts"]
                for variable in ["TASK_ID", "TASK_PROTOCOL_VERSION", "TASK_PAYLOAD", "SYNC_PAYLOAD_KEY", "GITHUB_RUN_ATTEMPT", "GITHUB_ACTIONS"]:
                    command.extend(["--env", variable])
                result = run(*command, image, "execute", "--output-dir", "/artifacts", env=env, check=False)
                public_output = "\n".join(line for line in result.stdout.splitlines() if not line.startswith("::add-mask::"))
                assert "fixture-private-password" not in public_output + result.stderr
                assert target_password not in public_output + result.stderr
                if key is not None:
                    assert result.returncode != 0 and "SYNC_PAYLOAD_KEY" in result.stderr
                    assert not (output_dir / "result.json").exists()
                    return
                expected_code = 0 if target_password == "fixture-private-password" else 1
                assert result.returncode == expected_code, public_output + result.stderr
                assert "::add-mask::fixture-private-password" in result.stdout
                artifact = output_dir / "result.json"
                assert artifact.stat().st_uid == runner_uid
                text = artifact.read_text()
                assert "fixture-private-password" not in text and target_password not in text
                log = (output_dir / "sync.log").read_text()
                assert "fixture-private-password" not in log and target_password not in log
                result = json.loads(text)
                assert result["task_id"] == task_id and result["run_attempt"] == 3
                assert len(result["items"]) == 3
                return result

            version = run("docker", "run", "--rm", "--entrypoint", "/usr/bin/skopeo", image, "--version").stdout
            assert "skopeo version" in version
            first = execute("first")
            assert first["summary"] == {"synced": 3, "unchanged": 0, "failed": 0}
            for label, expected in [("all", digest(index)), ("amd64", leaves["amd64"]), ("arm64", leaves["arm64"])]:
                with request(f"/v2/target/demo/manifests/{label}") as response:
                    assert digest(response.read()) == expected
                with request(f"/v2/target/demo/blobs/{layer_digest}") as response:
                    assert response.read() == layer
            second = execute("second")
            assert second["summary"] == {"synced": 0, "unchanged": 3, "failed": 0}
            rejected = execute("wrong-registry-password", target_password="incorrect-fixture-password")
            assert rejected["summary"] == {"synced": 0, "unchanged": 0, "failed": 3}
            execute("missing-key", "")
            execute("invalid-key", "github_pat_example-not-a-task-key")
            print(json.dumps({"real_skopeo": True, "tls_registry": True, "platforms": ["all", "amd64", "arm64"],
                              "layers_verified": True, "unchanged_skipped": True, "artifact_owner": runner_uid,
                              "private_registry_auth": True, "wrong_credentials_rejected": True,
                              "secret_masks_registered": True, "invalid_key_rejected": True}))
        finally:
            run("docker", "rm", "-f", registry_name, check=False)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    main(parser.parse_args().image)
