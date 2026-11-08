"""Install real npm/Python packages against a locally served release archive."""

from __future__ import annotations

import argparse
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import venv
import zipfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from package_release import ROOT, release_version


def run(command: list[str], env: dict[str, str], expected: int = 0) -> str:
    """Run a package command with bounded execution and useful failure output."""
    # cmd /s strips one outer quote pair; retain the quotes around each argument.
    invocation = (
        'cmd.exe /d /s /c "' + subprocess.list2cmdline(command[3:]) + '"'
        if command[0] == "cmd.exe"
        else command
    )
    result = subprocess.run(
        invocation, env=env, cwd=ROOT, capture_output=True, text=True, timeout=180, check=False
    )
    if result.returncode != expected:
        raise RuntimeError(
            f"{command} exited {result.returncode}\n{result.stdout}\n{result.stderr}"
        )
    return result.stdout


def verify(commands: list[list[str]], fixture: Path, env: dict[str, str]) -> None:
    """Exercise aliases, arguments with spaces, clean JSON stdout, and exit codes."""
    for command in commands:
        assert run(command + ["--version"], env).strip() == f"ast-bro {release_version()}"
        output = run(command + ["map", str(fixture), "--json"], env)
        assert "greet" in json.dumps(json.loads(output)), output
        run(command + ["--definitely-invalid"], env, expected=2)


def smoke(archive: Path, temp: Path) -> None:
    """Keep production download URLs intact; redirect transport only in test processes."""
    expected_url = (
        f"https://github.com/aeroxy/ast-bro/releases/download/{release_version()}/{archive.name}"
    )
    requests = []
    download_gates: dict[str, tuple[threading.Event, threading.Event]] = {}

    class Handler(BaseHTTPRequestHandler):
        """Serve the real ZIP through an HTTP redirect, like GitHub releases."""

        def do_GET(self) -> None:
            requests.append(self.path)
            gate = download_gates.get(self.path)
            if gate:
                entered, proceed = gate
                entered.set()
                if not proceed.wait(timeout=30):
                    self.send_error(504, "Timed out waiting for the concurrent installer")
                    return
            if self.path == "/download" or gate:
                self.send_response(302)
                self.send_header("Location", "/archive.zip")
                self.end_headers()
            elif self.path == "/archive.zip":
                self.send_response(200)
                self.send_header("Content-Length", str(archive.stat().st_size))
                self.end_headers()
                with archive.open("rb") as source:
                    shutil.copyfileobj(source, self.wfile)
            else:
                self.send_error(404)

        def log_message(self, format: str, *args: str) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_port}/download"

    # These hooks affect only the smoke-test children, never the shipped packages.
    hooks = temp / "hooks"
    hooks.mkdir()
    node_hook = hooks / "redirect.cjs"
    node_hook.write_text(
        """
const cp = require("node:child_process");
const os = require("node:os");
const path = require("node:path");
os.homedir = () => process.env.AST_BRO_SMOKE_HOME;
const execute = cp.execFileSync;
cp.execFileSync = (file, args, options) => {
  if (path.basename(file) === "curl.exe" || file === "curl") {
    if (args[args.length - 1] !== process.env.AST_BRO_EXPECTED_URL) {
      throw new Error("Wrong release URL: " + args[args.length - 1]);
    }
    args = [...args.slice(0, -1), process.env.AST_BRO_SMOKE_URL];
  }
  return execute(file, args, options);
};
""",
        encoding="utf-8",
    )
    (hooks / "sitecustomize.py").write_text(
        """
import os
from pathlib import Path
Path.home = classmethod(lambda cls: Path(os.environ["AST_BRO_SMOKE_HOME"]))
try:
    import httpx
except ImportError:
    pass
else:
    original = httpx.Client.get
    def redirected(self, url, *args, **kwargs):
        assert url == os.environ["AST_BRO_EXPECTED_URL"], url
        return original(self, os.environ["AST_BRO_SMOKE_URL"], *args, **kwargs)
    httpx.Client.get = redirected
""",
        encoding="utf-8",
    )
    env = dict(os.environ, NO_COLOR="1", AST_BRO_EXPECTED_URL=expected_url, AST_BRO_SMOKE_URL=url)
    env["NODE_OPTIONS"] = f'--require="{node_hook.as_posix()}"'
    env["PYTHONPATH"] = str(hooks)
    fixture = temp / "source file.py"
    fixture.write_text("def greet(name):\n    return name\n", encoding="utf-8")

    def fresh_cache(name: str) -> None:
        """Use paths with shell metacharacters to catch unsafe quoting."""
        cache = temp / name / "User's cache & [files]"
        cache.mkdir(parents=True)
        env["AST_BRO_SMOKE_HOME"] = str(cache)
        env["LOCALAPPDATA"] = str(cache)

    def concurrent_installs(first: list[str], second: list[str], name: str) -> None:
        """Make both installers miss the cache, then keep the winner's executable running."""
        fresh_cache(name)
        entered = [threading.Event(), threading.Event()]
        proceed = [threading.Event(), threading.Event()]
        processes = []
        try:
            for index, command in enumerate((first + ["mcp"], second + ["--version"])):
                route = f"/download/{index}"
                download_gates[route] = (entered[index], proceed[index])
                child_env = dict(env, AST_BRO_SMOKE_URL=f"{url}/{index}")
                processes.append(
                    subprocess.Popen(
                        command,
                        env=child_env,
                        cwd=ROOT,
                        stdin=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        text=True,
                    )
                )
                assert entered[index].wait(timeout=30), (
                    f"{name}: installer {index} did not download"
                )

            winner, loser = processes
            proceed[0].set()
            winner.stdin.write(
                json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}) + "\n"
            )
            winner.stdin.flush()
            responses = queue.Queue()
            threading.Thread(
                target=lambda: responses.put(winner.stdout.readline()), daemon=True
            ).start()
            response = json.loads(responses.get(timeout=30))
            assert response.get("id") == 1 and "result" in response, response

            # The second download resumes only after Windows has loaded the cached EXE.
            proceed[1].set()
            stdout, stderr = loser.communicate(timeout=60)
            assert loser.returncode == 0, f"{name}: {stderr}"
            assert stdout.strip() == f"ast-bro {release_version()}", stdout
            assert winner.poll() is None, f"{name}: the MCP process exited unexpectedly"
            print(f"Concurrent installation passed ({name}, winner still running)", flush=True)
        finally:
            for event in proceed:
                event.set()
            for process in processes:
                try:
                    process.communicate(timeout=30)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate(timeout=10)
            download_gates.clear()

    try:
        fresh_cache("native")
        native = temp / "native binaries"
        with zipfile.ZipFile(archive) as packed:
            packed.extractall(native)
        suffix = ".exe" if os.name == "nt" else ""
        binaries = [native / f"{name}{suffix}" for name in ("ast-bro", "ast-outline", "sb")]
        for binary in binaries:
            binary.chmod(0o755)
        verify([[str(binary)] for binary in binaries], fixture, env)
        print("Native archive and all three aliases passed", flush=True)

        node = shutil.which("node")
        npm_path = shutil.which("npm")
        if not node or not npm_path:
            raise RuntimeError("The package smoke test requires Node.js and npm")
        npm = Path(npm_path).resolve()
        if os.name == "nt":
            npm = npm.parent / "node_modules/npm/bin/npm-cli.js"
        npm_command = [node, str(npm)]
        package_info = json.loads(
            run(
                npm_command
                + ["pack", "./cli-typescript", "--pack-destination", str(temp), "--json"],
                env,
            )
        )
        tarball = temp / package_info[0]["filename"]
        for skip_scripts in (False, True):
            fresh_cache(f"npm-{skip_scripts}")
            prefix = temp / f"npm-{skip_scripts}"
            flags = ["--ignore-scripts"] if skip_scripts else ["--foreground-scripts"]
            run(
                npm_command
                + [
                    "install",
                    "-g",
                    "--prefix",
                    str(prefix),
                    "--no-audit",
                    "--no-fund",
                    *flags,
                    str(tarball),
                ],
                env,
            )
            commands = []
            for name in ("ast-bro", "ast-outline", "sb"):
                if os.name == "nt":
                    commands.append(["cmd.exe", "/d", "/c", str(prefix / f"{name}.cmd")])
                else:
                    commands.append([str(prefix / "bin" / name)])
            before = len(requests)
            verify(commands, fixture, env)
            assert len(requests) == before + (2 if skip_scripts else 0), requests
            print(f"npm tarball passed (skip postinstall: {skip_scripts})", flush=True)

        fresh_cache("python")
        dist = temp / "dist"
        run([sys.executable, "-m", "build", "--wheel", "--outdir", str(dist), "cli-python"], env)
        install_dir = temp / "python install"
        venv.EnvBuilder(with_pip=True).create(install_dir)
        bin_dir = install_dir / ("Scripts" if os.name == "nt" else "bin")
        python = bin_dir / ("python.exe" if os.name == "nt" else "python")
        run(
            [
                str(python),
                "-m",
                "pip",
                "install",
                "--disable-pip-version-check",
                str(next(dist.glob("*.whl"))),
            ],
            env,
        )
        before = len(requests)
        verify(
            [[str(bin_dir / f"{name}{suffix}")] for name in ("ast-bro", "ast-outline", "sb")],
            fixture,
            env,
        )
        assert len(requests) == before + 2, requests
        print("Python wheel passed (first-run download and cached reruns)", flush=True)
        if os.name == "nt":
            node_cli = [node, str(prefix / "node_modules/@ast-bro/cli/bin/ast-bro.js")]
            python_cli = [str(python), "-m", "ast_bro_cli"]
            for first, second, name in (
                (node_cli, node_cli, "npm-npm"),
                (python_cli, python_cli, "python-python"),
                (node_cli, python_cli, "npm-python"),
                (python_cli, node_cli, "python-npm"),
            ):
                concurrent_installs(first, second, name)
    finally:
        server.shutdown()
        server.server_close()


def main() -> None:
    """Build/install packages in a disposable directory below target/."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    args = parser.parse_args()
    (ROOT / "target").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="package smoke ", dir=ROOT / "target") as directory:
        smoke(args.archive.resolve(), Path(directory))


if __name__ == "__main__":
    main()
