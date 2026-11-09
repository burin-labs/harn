#!/usr/bin/env python3
"""Exercise stage-zero transport with real downloaders and local HTTPS redirects."""

import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest


BOOTSTRAP = Path(os.environ.get(
    "HARN_SEED_TEST_BOOTSTRAP_SCRIPT",
    str(Path(__file__).resolve().parents[1] / "bootstrap-harn.sh"),
))
ASSET = "harn-x86_64-unknown-linux-gnu.tar.xz"
TOKEN = "synthetic-bootstrap-transport-token"


class SeedTransportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="harn-seed-auth-")
        self.root = Path(self.temporary.name)
        archive = io.BytesIO()
        seed = (
            b'#!/bin/sh\nfor path in "$TMPDIR"/harn-seed.*/api.headers "$TMPDIR"/harn-seed.*/api.wget; do\n'
            b'  [ ! -f "$path" ] || exit 41\ndone\nprintf "seed-reached\\n"\n'
        )
        with tarfile.open(fileobj=archive, mode="w:xz") as output:
            member = tarfile.TarInfo("harn")
            member.mode = 0o755
            member.size = len(seed)
            output.addfile(member, io.BytesIO(seed))
        self.archive = archive.getvalue()
        self.manifest = f"{hashlib.sha256(self.archive).hexdigest()}  {ASSET}\n".encode()
        self.requests = []
        self.fail_direct = False
        self.deny_api = False
        self.fail_all = False
        self.require_user_agent = False
        self.empty_default_agent = False
        self.port = 0
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                authenticated = self.headers.get("Authorization") == f"Bearer {TOKEN}"
                agent = self.headers.get("User-Agent")
                owner.requests.append((self.path, authenticated, "Authorization" in self.headers, agent))
                if owner.require_user_agent and not agent:
                    self.send_error(403, "User-Agent required")
                    return
                api = self.path.startswith("/repos/burin-labs/harn/releases/")
                direct = self.path.startswith("/burin-labs/harn/releases/download/")
                if owner.fail_all or (owner.fail_direct and direct):
                    self.send_error(503)
                    return
                if api and owner.deny_api:
                    self.send_response(403)
                    # A denial must not turn into an anonymous asset redirect.
                    self.send_header("Location", f"https://localhost:{owner.port}/cdn/101")
                    self.end_headers()
                    return
                if self.path.endswith("/tags/v9.8.7"):
                    body = json.dumps({"assets": [
                        {"name": "SHA256SUMS", "id": 101},
                        {"name": ASSET, "id": 102},
                    ]}).encode()
                elif api and "/assets/" in self.path:
                    self.send_response(302)
                    self.send_header("Location", f"https://localhost:{owner.port}/cdn/{self.path.rsplit('/', 1)[1]}")
                    self.end_headers()
                    return
                elif self.path.endswith(("/SHA256SUMS", "/cdn/101")):
                    body = owner.manifest
                elif self.path.endswith((f"/{ASSET}", "/cdn/102")):
                    body = owner.archive
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        certificate = self.root / "certificate.pem"
        key = self.root / "key.pem"
        subprocess.run([
            shutil.which("openssl"), "req", "-x509", "-newkey", "rsa:2048", "-nodes",
            "-keyout", str(key), "-out", str(certificate), "-days", "1", "-subj", "/CN=localhost",
        ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_port
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(certificate, key)
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.temporary.cleanup()

    def modes(self):
        for mode in ["curl", "wget"]:
            if shutil.which(mode):
                yield mode

    def tools(self, mode):
        tools = self.root / mode / "tools"
        tools.mkdir(parents=True, exist_ok=True)
        for name in ["sh", "mkdir", "mktemp", "dirname", "cat", "grep", "tr", "sort",
                     "head", "rm", "awk", "cmp", "cp", "ln", "mv", "tar", "chmod",
                     "sha256sum", "shasum", "jq", "xz", "gzip"]:
            executable = shutil.which(name)
            if executable and not (tools / name).exists():
                (tools / name).symlink_to(executable)
        uname = tools / "uname"
        uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
        uname.chmod(0o755)
        # Preserve production URLs until the real downloader executes. The
        # fixture changes only the endpoint and TLS trust. One regression also
        # clears the downloader's default agent so production must supply it.
        wrapper = tools / mode
        extra = ["--insecure", "--noproxy", "*"] if mode == "curl" else ["--no-check-certificate", "--no-proxy"]
        if self.empty_default_agent:
            extra += ["--user-agent", ""]
        wrapper.write_text(
            f"#!{sys.executable}\n"
            "import os, sys\n"
            "from pathlib import Path\n"
            "from urllib.parse import urlsplit\n"
            "arguments = sys.argv[1:]\n"
            f"assert not any({TOKEN!r} in argument for argument in arguments), 'token in process arguments'\n"
            "headers = [argument[1:] for argument in arguments if argument.startswith('@')]\n"
            "headers += [argument.split('=', 1)[1] for argument in arguments if argument.startswith('--config=')]\n"
            "assert all(Path(path).stat().st_mode & 0o777 == 0o600 for path in headers), 'credential file is not private'\n"
            "for index, argument in enumerate(arguments):\n"
            "    if argument.startswith(('https://api.github.com/', 'https://github.com/')):\n"
            f"        arguments[index] = 'https://127.0.0.1:{self.port}' + urlsplit(argument).path\n"
            f"extra = {extra!r}\n"
            f"os.execv({shutil.which(mode)!r}, [{mode!r}] + extra + arguments)\n"
        )
        wrapper.chmod(0o755)
        return tools

    def run_seed(self, mode, authenticated=False, token=None):
        environment = {
            "PATH": str(self.tools(mode)),
            "HOME": str(self.root / "home"),
            "TMPDIR": str(self.root),
            "HARN_EXT_BOOTSTRAP_SEED_CACHE": str(self.root / mode / "cache"),
            "HARN_EXT_BOOTSTRAP_SEED_VERSION": "9.8.7",
            "HARN_BOOTSTRAP_TOKEN": token if token is not None else TOKEN if authenticated else "",
        }
        result = subprocess.run(
            ["/bin/sh", str(BOOTSTRAP), "--seed-command", "--version"],
            env=environment, capture_output=True, timeout=45,
        )
        self.assertFalse(TOKEN.encode() in result.stdout + result.stderr, "token leaked in bootstrap output")
        return result

    def cache(self, mode):
        return self.root / mode / "cache/9.8.7/x86_64-unknown-linux-gnu"

    def assert_seed(self, result):
        detail = result.stderr.decode(errors="replace").replace(TOKEN, "[redacted]")
        self.assertEqual(result.returncode, 0, f"verified seed did not execute: {detail}; requests={self.requests}")
        self.assertEqual(result.stdout, b"seed-reached\n")

    def test_authenticated_api_redirects_and_warm_cache(self):
        self.fail_direct = True
        for mode in self.modes():
            with self.subTest(mode=mode):
                self.requests.clear()
                self.assert_seed(self.run_seed(mode, authenticated=True))
                api = [row for row in self.requests if row[0].startswith("/repos/")]
                other = [row for row in self.requests if not row[0].startswith("/repos/")]
                self.assertEqual(len(api), 3)
                self.assertEqual(len(other), 2)
                self.assertTrue(all(row[1] for row in api), "API request lacked its token")
                self.assertTrue(all(not row[2] for row in other), "token forwarded off the API origin")
                self.assertTrue(all(row[0].startswith("/cdn/") for row in other), "authenticated bootstrap attempted direct downloads")
                self.fail_all = True
                self.requests.clear()
                self.assert_seed(self.run_seed(mode, authenticated=True))
                self.assertEqual(self.requests, [], "verified cache contacted the network")
                self.fail_all = False

    def test_every_origin_gets_an_agent_without_downloader_defaults(self):
        self.require_user_agent = True
        self.empty_default_agent = True
        modes = list(self.modes())
        self.assertTrue(modes, "no downloader was exercised")
        for mode in modes:
            for authenticated, fail_direct in [(False, False), (False, True), (True, True)]:
                with self.subTest(mode=mode, authenticated=authenticated, fail_direct=fail_direct):
                    shutil.rmtree(self.root / mode / "cache", ignore_errors=True)
                    self.requests.clear()
                    self.fail_direct = fail_direct
                    self.assert_seed(self.run_seed(mode, authenticated=authenticated))
                    self.assertGreater(len(self.requests), 0, "header check measured no requests")
                    self.assertTrue(all(row[3] == "harn-bootstrap" for row in self.requests))
                    if fail_direct:
                        self.assertTrue(any(row[0].startswith("/repos/") for row in self.requests))
                        self.assertTrue(any(row[0].startswith("/cdn/") for row in self.requests))
                    else:
                        self.assertTrue(all(row[0].startswith("/burin-labs/") for row in self.requests))
                    self.assertTrue(all(not row[2] for row in self.requests if not row[0].startswith("/repos/")))

    def test_anonymous_failover_uses_one_direct_attempt(self):
        self.fail_direct = True
        for mode in self.modes():
            with self.subTest(mode=mode):
                self.requests.clear()
                self.assert_seed(self.run_seed(mode))
                self.assertEqual(sum(row[0].startswith("/burin-labs/") for row in self.requests), 1)
                self.assertEqual(sum(row[0].startswith("/repos/") for row in self.requests), 3)
                self.assertTrue(all(not row[2] for row in self.requests))

    def test_verified_anonymous_cache_has_zero_network_calls(self):
        for mode in self.modes():
            with self.subTest(mode=mode):
                self.assert_seed(self.run_seed(mode))
                self.assertGreater(len(self.requests), 0, "cold network meter did not fire")
                self.requests.clear()
                self.fail_all = True
                self.assert_seed(self.run_seed(mode))
                self.assertEqual(self.requests, [])
                self.fail_all = False

    def test_denial_does_not_downgrade_authentication(self):
        self.deny_api = True
        for mode in self.modes():
            with self.subTest(mode=mode):
                self.requests.clear()
                result = self.run_seed(mode, authenticated=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(self.requests), 1)
                self.assertTrue(self.requests[0][1])
                self.assertFalse((self.cache(mode) / ASSET).exists())

    def test_token_control_characters_refuse_before_network(self):
        for mode in self.modes():
            for suffix in ["\nInjected: value", "\r", "\t"]:
                with self.subTest(mode=mode, control=repr(suffix)):
                    self.requests.clear()
                    result = self.run_seed(mode, token=TOKEN + suffix)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(b"invalid bootstrap token", result.stderr)
                    self.assertEqual(self.requests, [])

    def test_fetched_metadata_cannot_replace_cached_checksums(self):
        for mode in self.modes():
            with self.subTest(mode=mode):
                original = self.manifest
                self.assert_seed(self.run_seed(mode, authenticated=True))
                (self.cache(mode) / ASSET).unlink()
                self.manifest += b"0" * 64 + b"  another-asset\n"
                result = self.run_seed(mode, authenticated=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"published seed checksums changed", result.stderr)
                self.assertEqual((self.cache(mode) / "SHA256SUMS").read_bytes(), original)
                self.assertFalse((self.cache(mode) / ASSET).exists())
                self.manifest = original


if __name__ == "__main__":
    for required in ["jq", "openssl"]:
        if not shutil.which(required):
            raise SystemExit(f"missing required transport-test tool: {required}")
    available = [mode for mode in ["curl", "wget"] if shutil.which(mode)]
    if not available:
        raise SystemExit("no real downloader available for transport tests")
    if sys.platform.startswith("linux") and available != ["curl", "wget"]:
        raise SystemExit("Linux transport verification requires both curl and wget")
    print(f"Real seed transport coverage: {', '.join(available)}", flush=True)
    unittest.main()
