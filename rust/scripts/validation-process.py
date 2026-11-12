# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Supervise one owned validation process tree, with file-backed output."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any


def supervise(config: dict[str, Any], pending_control: bytes = b"") -> dict[str, Any]:
    """Run a bounded command and confirm cleanup before publishing its outcome."""
    stopped = threading.Event()
    reason: list[str] = []

    def cancel(value: str) -> None:
        if not reason:
            reason.append(value)
        stopped.set()

    def control() -> None:
        # EOF also means the Node owner disappeared; never leave its command alive.
        value = pending_control[:1] or os.read(sys.stdin.fileno(), 1)
        cancel("timeout" if value == b"t" else "cancelled")

    threading.Thread(target=control, daemon=True).start()
    for name in ("SIGTERM", "SIGINT"):
        signal.signal(getattr(signal, name), lambda *_: cancel("cancelled"))
    tree = None
    if os.name != "nt":
        from validation_posix import DarwinTree, LinuxTree

        tree = LinuxTree() if sys.platform.startswith("linux") else DarwinTree()
    deadline = (
        time.monotonic()
        + max(0, min(config["timeout_ms"], config["deadline_ms"] - time.time() * 1000))
        / 1000
    )
    child: subprocess.Popen[bytes] | None = None
    job = None
    cleanup: list[str] = []
    outcome: dict[str, Any] = {
        "status": None,
        "signal": None,
        "timed_out": False,
        "cancelled": False,
        "error": None,
        "cleanup_confirmed": False,
    }
    try:
        options: dict[str, Any] = {"stdin": subprocess.DEVNULL}
        environment = config["env"]
        marker = getattr(tree, "environment", None)
        if marker:
            environment = {
                **(os.environ if environment is None else environment),
                **marker,
            }
        if os.name == "nt":
            # Reuse the native acceptance boundary: suspended launch, assign to
            # kill-on-close Job, then resume the unique primary thread.
            from native_acceptance import WindowsTree

            job = WindowsTree()
            options["creationflags"] = 0x4 | subprocess.CREATE_NEW_PROCESS_GROUP
        else:
            options["start_new_session"] = True
        if stopped.is_set() or time.monotonic() >= deadline:
            cancel("cancelled" if stopped.is_set() else "timeout")
        else:
            with (
                open(config["stdout"], "wb", buffering=0) as stdout,
                open(config["stderr"], "wb", buffering=0) as stderr,
            ):
                child = subprocess.Popen(
                    [config["program"], *config["args"]],
                    cwd=config["cwd"],
                    env=environment,
                    stdout=stdout,
                    stderr=stderr,
                    **options,
                )
                outcome["pid"] = child.pid
                print(json.dumps({"pid": child.pid}), flush=True)
                if job:
                    job.attach(child, deadline)
                if tree:
                    tree.observe(child.pid)
                while child.poll() is None and not stopped.is_set():
                    if tree:
                        tree.observe(child.pid)
                    if time.monotonic() >= deadline:
                        cancel("timeout")
                        break
                    stopped.wait(min(0.025, max(0, deadline - time.monotonic())))
    except Exception as error:
        outcome["error"] = f"{type(error).__name__}: {error}"
        if time.monotonic() >= deadline:
            cancel("timeout")
    finally:
        if reason:
            outcome["timed_out"] = reason[0] == "timeout"
            outcome["cancelled"] = reason[0] == "cancelled"
            outcome["error"] = (
                "ETIMEDOUT: command budget exhausted"
                if outcome["timed_out"]
                else "ECANCELLED: validation cancelled"
            )
        cleanup_deadline = time.monotonic() + 5
        if job:
            if not job.close():
                cleanup.append("job_tree_cleanup_unconfirmed")
        elif child and tree:
            cleanup.extend(tree.terminate(child.pid, cleanup_deadline))
        if child:
            try:
                code = child.wait(timeout=2)
                outcome["status"] = code if code >= 0 else None
                outcome["signal"] = signal.Signals(-code).name if code < 0 else None
            except subprocess.TimeoutExpired:
                child.kill()  # Specific owned PID, including a failed suspended launch.
                try:
                    child.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    cleanup.append("owned_child_reap_unconfirmed")
        if tree and child:
            cleanup.extend(tree.confirm(cleanup_deadline))
        outcome["cleanup_method"] = tree.method if tree else "windows-job-object"
        cleanup = list(dict.fromkeys(cleanup))
        outcome["cleanup_confirmed"] = not cleanup
        outcome["cleanup_errors"] = cleanup
        if cleanup and not outcome["error"]:
            outcome["error"] = "; ".join(cleanup)
    return outcome


def main() -> None:
    """Read one private command configuration and persist its typed outcome."""
    data = b""
    while b"\n" not in data:
        chunk = os.read(sys.stdin.fileno(), 65536)
        if not chunk:
            raise RuntimeError("Validation owner closed before sending configuration")
        data += chunk
    raw, _, pending_control = data.partition(b"\n")
    config = json.loads(raw)
    outcome = supervise(config, pending_control)
    result = Path(config["result"])
    partial = result.with_name(result.name + ".partial")
    partial.write_text(json.dumps(outcome), encoding="utf-8")
    partial.replace(result)


if __name__ == "__main__":
    main()
