# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise the actual Rust deployment adapter under an offline Unix PTY."""

import errno
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import sys
import time
import venv

import psutil

FAKE_MAIN = r"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def main() -> None:
    assert sys.argv[0] == "fab", sys.argv
    assert sys.argv[1] == "deploy", sys.argv
    assert "--force" not in sys.argv, sys.argv
    mode = sys.argv[sys.argv.index("--target_env") + 1]
    config = Path(sys.argv[sys.argv.index("--config") + 1])
    assert config.name == f"{mode} config '東京'.json", config
    assert sys.stdin.isatty() and sys.stdout.isatty() and sys.stderr.isatty()
    pids = [os.getpid()]
    if mode == "group-terminate":
        ready = config.with_suffix(".ready")
        child = subprocess.Popen(
            [
                sys.executable, "-I", "-B", "-c",
                "import pathlib,signal,sys,time;"
                "signal.signal(signal.SIGINT,signal.SIG_IGN);"
                "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
                "pathlib.Path(sys.argv[1]).write_text('ready');"
                "time.sleep(60)",
                str(ready),
            ],
            start_new_session=True,
        )
        deadline = time.monotonic() + 5
        while not ready.exists():
            assert child.poll() is None, "descendant exited before signal test"
            assert time.monotonic() < deadline, "descendant did not become ready"
            time.sleep(0.01)
        pids.append(child.pid)
    config.write_text(json.dumps(pids), encoding="utf-8")
    answer = input("DEPLOY_CONFIRM> ")
    assert os.tcgetpgrp(sys.stdin.fileno()) == os.getpgrp()
    print("CONFIRMED" if answer == "yes" else "DECLINED", flush=True)
    raise SystemExit(17 if answer == "yes" else 0)
"""


def install_fixture(root: Path) -> Path:
    """Provision fake installed product packages without pip or network access."""
    environment = root / "python environment"
    venv.EnvBuilder(system_site_packages=True, with_pip=False).create(environment)
    python = environment / "bin" / "python"
    purelib = Path(
        subprocess.check_output(
            [
                str(python),
                "-I",
                "-B",
                "-c",
                "import sysconfig; print(sysconfig.get_path('purelib'))",
            ],
            text=True,
        ).strip()
    )
    for package in [
        "fabric_cli",
        "fabric_cli/commands",
        "fabric_cli/commands/fs",
        "fabric_cicd",
    ]:
        directory = purelib / package
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "__init__.py").write_text("", encoding="utf-8")
    (purelib / "fabric_cli" / "main.py").write_text(FAKE_MAIN, encoding="utf-8")
    (purelib / "fabric_cli" / "commands" / "fs" / "fab_fs_deploy.py").write_text(
        "", encoding="utf-8"
    )
    (purelib / "fabric_cicd" / "__init__.py").write_text(
        "def deploy_with_config():\n    raise AssertionError('no live deployment')\n",
        encoding="utf-8",
    )
    for name in ["ms_fabric_cli", "fabric_cicd"]:
        metadata = purelib / f"{name}-0.0.0.dist-info"
        metadata.mkdir()
        (metadata / "METADATA").write_text(
            f"Metadata-Version: 2.1\nName: {name.replace('_', '-')}\nVersion: 0.0.0\n",
            encoding="utf-8",
        )
    return python


def read_terminal(master: int) -> bytes:
    """Treat the Unix PTY's EIO-at-close convention as end of stream."""
    try:
        return os.read(master, 65536)
    except OSError as error:
        if error.errno != errno.EIO:
            raise
        return b""


def stop_fixture(pid: int, known: list[psutil.Process], reaped: bool) -> None:
    """Also stop recorded descendants if a broken adapter orphaned them."""
    processes = list(known)
    if not reaped:
        try:
            processes.extend(psutil.Process(pid).children(recursive=True))
        except psutil.NoSuchProcess:
            pass
    for process in processes:
        try:
            process.kill()
        except psutil.NoSuchProcess:
            pass
    if not reaped:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
    psutil.wait_procs(processes, timeout=2)


def exercise(binary: str, python: Path, root: Path, mode: str) -> None:
    """Confirm, decline, or cancel by terminal input or foreground group signal."""
    config = root / f"{mode} config '東京'.json"
    environment = os.environ.copy()
    environment.update(
        FAB_RUST_DEPLOY_PYTHON=str(python),
        FAB_RUST_CONFIG_ROOT="invalid-native-config-secret",
        FAB_RUST_TENANT="invalid-native-tenant-secret",
    )
    pid, master = pty.fork()
    if pid == 0:
        os.execve(
            binary,
            [binary, "deploy", "--config", str(config), "--target_env", mode],
            environment,
        )

    output = bytearray()
    status = None
    action_sent = False
    known: list[psutil.Process] = []
    try:
        deadline = time.monotonic() + 15
        while status is None:
            if select.select([master], [], [], 0.05)[0]:
                output.extend(read_terminal(master))
            if not action_sent and b"DEPLOY_CONFIRM> " in output:
                deployment_pids = json.loads(config.read_text(encoding="utf-8"))
                known = [psutil.Process(process) for process in deployment_pids]
                assert all(process.is_running() for process in known)
                assert os.getpgid(known[0].pid) == pid
                if mode == "group-terminate":
                    assert len(known) == 2, deployment_pids
                    assert known[1].ppid() == known[0].pid
                    assert os.getpgid(known[1].pid) != pid
                    # The descendant survives the group signal unless the
                    # adapter discovers and kills it through psutil.
                    os.killpg(pid, signal.SIGTERM)
                else:
                    # This is terminal input, not a pipe or a direct signal to Rust.
                    response = {
                        "accept": b"yes\n",
                        "decline": b"no\n",
                        "cancel": b"\x03",
                    }[mode]
                    os.write(master, response)
                action_sent = True
            waited, child_status = os.waitpid(pid, os.WNOHANG)
            if waited:
                status = child_status
                while select.select([master], [], [], 0)[0]:
                    chunk = read_terminal(master)
                    if not chunk:
                        break
                    output.extend(chunk)
            assert (
                time.monotonic() < deadline
            ), f"{mode}: adapter stalled reading its controlling terminal: {output!r}"
        assert action_sent, f"{mode}: did not reach confirmation: {output!r}"
        expected = {"accept": 17, "decline": 0, "cancel": 2, "group-terminate": 2}[mode]
        assert os.waitstatus_to_exitcode(status) == expected, (mode, status, output)
        if mode in ["cancel", "group-terminate"]:
            assert b"OperationCancelled" in output, output
            assert b"process-tree cleanup could not be verified" not in output, output
            assert b"CONFIRMED" not in output and b"DECLINED" not in output, output
        else:
            assert (b"CONFIRMED" if mode == "accept" else b"DECLINED") in output, output
        assert known, f"{mode}: no deployment process recorded"
        for process in known:
            assert not process.is_running(), (mode, process.pid, output)
    finally:
        try:
            stop_fixture(pid, known, status is not None)
        finally:
            os.close(master)


def main() -> None:
    """Run only synthetic installed modules; never import the real product."""
    binary = str(Path(sys.argv[1]).resolve())
    root = Path(sys.argv[2]).resolve()
    root.mkdir(parents=True, exist_ok=False)
    try:
        python = install_fixture(root)
        for mode in ["accept", "decline", "cancel", "group-terminate"]:
            exercise(binary, python, root, mode)
    finally:
        shutil.rmtree(root)


if __name__ == "__main__":
    main()
