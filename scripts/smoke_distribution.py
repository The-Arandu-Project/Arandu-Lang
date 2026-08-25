#!/usr/bin/env python3
"""S3-C smoke: exercise only an installed Arandu SDK."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def run(command: list[str], cwd: Path, expected: int = 0) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    for name in ("ARANDU_STDLIB", "CARGO", "CARGO_HOME", "RUSTUP_HOME"):
        env.pop(name, None)
    if os.name == "nt":
        system_root = env.get("SystemRoot", r"C:\Windows")
        env["PATH"] = os.pathsep.join([str(Path(system_root) / "System32"), system_root])
    else:
        env["PATH"] = "/usr/bin:/bin"
    result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True, timeout=60)
    if result.returncode != expected:
        raise SystemExit(
            f"command failed ({result.returncode}, expected {expected}): {command}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result


def frame(message: dict) -> bytes:
    payload = json.dumps(message, separators=(",", ":")).encode()
    return f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload


def lsp_smoke(server: Path, cwd: Path) -> None:
    process = subprocess.Popen(
        [str(server)], cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )

    def send(message: dict) -> None:
        assert process.stdin is not None
        process.stdin.write(frame(message))
        process.stdin.flush()

    def receive(expected_id: int) -> None:
        assert process.stdout is not None
        while True:
            length = None
            while True:
                line = process.stdout.readline()
                if not line:
                    raise SystemExit(f"LSP closed before response {expected_id}")
                if line == b"\r\n":
                    break
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":", 1)[1].strip())
            if length is None:
                raise SystemExit("LSP response missing Content-Length")
            response = json.loads(process.stdout.read(length))
            if "method" in response and "id" not in response:
                continue
            if response.get("id") != expected_id or "error" in response:
                raise SystemExit(f"unexpected LSP response: {response!r}")
            return

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}})
        receive(1)
        send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        send({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": None})
        receive(2)
        send({"jsonrpc": "2.0", "method": "exit", "params": None})
        assert process.stdin is not None
        process.stdin.close()
        if process.wait(timeout=10) != 0:
            raise SystemExit(f"LSP exited with {process.returncode}")
    finally:
        if process.poll() is None:
            process.kill()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arandu", required=True, type=Path)
    parser.add_argument("--lsp", required=True, type=Path)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    arandu = args.arandu.resolve()
    lsp = args.lsp.resolve()
    if not arandu.is_file() or not lsp.is_file():
        raise SystemExit("installed CLI/LSP executable is missing")

    with tempfile.TemporaryDirectory(prefix="arandu-s3c-") as directory:
        work = Path(directory)
        version = run([str(arandu), "--version"], work)
        if version.stdout.strip() != f"arandu {args.expected_version}":
            raise SystemExit(
                f"installed CLI version mismatch: expected arandu {args.expected_version!s}, "
                f"found {version.stdout.strip()!r}"
            )
        run([str(arandu), "doctor"], work)
        created = run([str(arandu), "new", "installed_app", "--vcs=none"], work)
        if "arandu check" not in created.stdout or "arandu_cli" in created.stdout:
            raise SystemExit("new scaffold exposes an internal command name")
        project = work / "installed_app"
        run([str(arandu), "check"], project)
        run([str(arandu), "run"], project)
        run([str(arandu), "build"], project)
        build_states = list((project / "target").rglob("build-state.json"))
        if len(build_states) != 1:
            raise SystemExit(f"expected one native build state, found {build_states!r}")
        build_state = json.loads(build_states[0].read_text(encoding="utf-8"))
        artifact = build_states[0].parent / build_state["artifact"]
        if not artifact.is_file() or build_state.get("backend") != "cranelift-aot":
            raise SystemExit(f"installed SDK did not publish a native artifact: {build_state!r}")
        run([str(artifact)], project)
        run([str(arandu), "fmt", "src/main.aru"], project)
        lsp_smoke(lsp, project)

        corpus = work / "corpus"
        shutil.copytree(args.corpus.resolve(), corpus)
        run([str(arandu), "check"], corpus / "small/hello")
        run([str(arandu), "run"], corpus / "small/hello", expected=42)
        run([str(arandu), "check"], corpus / "medium/language_mix")
        run([str(arandu), "run"], corpus / "medium/language_mix", expected=42)
        cycle = run([str(arandu), "check"], corpus / "adversarial/import_cycle", expected=1)
        if "N006" not in cycle.stderr:
            raise SystemExit("installed corpus diagnostic lost expected N006")
        invalid = run([str(arandu), "check"], corpus / "adversarial/unicode_type_error", expected=1)
        if "T002" not in invalid.stderr:
            raise SystemExit("installed corpus diagnostic lost expected T002")
    print("S3 distribution smoke: ok")


if __name__ == "__main__":
    main()
