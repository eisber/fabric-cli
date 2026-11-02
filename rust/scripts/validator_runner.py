# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run one explicit request only after verifying the enclosing cgroup limits."""

from __future__ import annotations

import fcntl
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path


def validate_limits(limits: dict[str, str]) -> None:
    """Reject missing, unlimited, or larger-than-authorized cgroup limits."""
    quota, period = map(int, limits["cpu.max"].split())
    if period <= 0 or quota > period * 2 or quota <= 0:
        raise RuntimeError(f"CPU quota is not bounded to two CPUs: {limits}")
    if not 0 < int(limits["memory.max"]) <= 6 * 1024**3:
        raise RuntimeError(f"Memory limit exceeds authorization: {limits}")
    if limits["memory.swap.max"] != "0" or not 0 < int(limits["pids.max"]) <= 128:
        raise RuntimeError(f"Swap/task limits are not bounded: {limits}")


def main() -> int:
    """Verify containment and retain an exact command receipt."""
    active_started = time.monotonic()
    root = Path(os.environ["VALIDATOR_ROOT"]).resolve(strict=True)
    filesystem = subprocess.check_output(
        ["stat", "-f", "-c", "%T", str(root)], text=True
    ).strip()
    if filesystem not in ("ext2/ext3", "ext4", "btrfs", "xfs"):
        raise RuntimeError(f"Native Linux filesystem required; found {filesystem}")
    lock = (root / "validator.lock").open("a")
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    request = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8-sig"))
    run_id = sys.argv[2]
    if not re.fullmatch(r"[a-f0-9]{32}", run_id):
        raise ValueError("Invalid run ID")
    if request["schema_version"] != 1 or not 1 <= request["timeout_seconds"] <= 600:
        raise ValueError("Invalid request schema or timeout")
    argv = request["argv"]
    if (
        not isinstance(argv, list)
        or not argv
        or not all(isinstance(a, str) for a in argv)
    ):
        raise ValueError("argv must be a nonempty string array")
    cwd = (root / request["working_directory"]).resolve(strict=True)
    if not cwd.is_relative_to(root):
        raise ValueError("Working directory must stay in the validator root")
    group = Path("/sys/fs/cgroup") / Path(
        Path("/proc/self/cgroup").read_text().strip().split("::", 1)[1]
    ).relative_to("/")
    limits = {
        name: (group / name).read_text().strip()
        for name in ("cpu.max", "memory.max", "memory.swap.max", "pids.max")
    }
    validate_limits(limits)
    unit_properties = subprocess.check_output(
        [
            "systemctl",
            "show",
            group.name,
            "--property=RuntimeMaxUSec,CPUQuotaPerSecUSec,MemoryMax,MemorySwapMax,TasksMax,KillMode",
        ],
        text=True,
    ).strip()
    receipt = {
        "schema_version": 1,
        "run_id": run_id,
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "uid": os.getuid(),
        "cwd": str(cwd),
        "argv": argv,
        "cgroup": str(group),
        "limits": limits,
        "unit_properties": unit_properties,
        "filesystem": filesystem,
    }
    source_sha = request.get("source_sha")
    if source_sha is not None:
        if not re.fullmatch(r"[a-f0-9]{40}", source_sha):
            raise ValueError("source_sha must be a full lowercase commit SHA")
        actual = subprocess.check_output(
            ["git", "-C", str(cwd), "rev-parse", "HEAD"], text=True
        ).strip()
        dirty = subprocess.check_output(
            ["git", "-C", str(cwd), "status", "--porcelain"], text=True
        ).strip()
        if actual != source_sha or dirty:
            raise RuntimeError(
                f"Expected clean exact source {source_sha}; "
                f"actual {actual}; dirty={bool(dirty)}"
            )
        receipt["source_sha"] = actual
        os.environ["CARGO_TARGET_DIR"] = str(root / "targets" / source_sha)
    receipt["environment"] = {
        key: os.environ[key]
        for key in (
            "CARGO_HOME",
            "RUSTUP_HOME",
            "CARGO_BUILD_JOBS",
            "RUST_TEST_THREADS",
        )
    }
    budget_path = root / "active-budget.json"
    budget = (
        json.loads(budget_path.read_text())
        if budget_path.exists()
        else {"active_seconds": 0.0, "limit_seconds": 3600}
    )
    recorded_seconds = sum(
        json.loads(path.read_text()).get(
            "active_elapsed_seconds",
            json.loads(path.read_text()).get("elapsed_seconds", 0.0),
        )
        for path in (root / "logs").glob("*.json")
    )
    previous_seconds = max(budget["active_seconds"], recorded_seconds)
    if previous_seconds + request["timeout_seconds"] > budget["limit_seconds"]:
        raise RuntimeError("Initial 60-minute active budget cannot cover this command")
    # Charge the full bound first so a killed command cannot erase its usage.
    budget["active_seconds"] = previous_seconds + request["timeout_seconds"]
    budget_path.write_text(json.dumps(budget) + "\n")
    print(json.dumps(receipt), flush=True)
    started = time.monotonic()
    outcome = subprocess.run(argv, cwd=cwd, check=False)
    receipt["exit_code"] = outcome.returncode
    receipt["elapsed_seconds"] = round(time.monotonic() - started, 3)
    receipt["usage"] = {
        name: (group / name).read_text().strip()
        for name in ("memory.peak", "memory.events", "cpu.stat", "pids.current")
    }
    receipt["active_elapsed_seconds"] = round(time.monotonic() - active_started, 3)
    budget["active_seconds"] = previous_seconds + receipt["active_elapsed_seconds"]
    budget_path.write_text(json.dumps(budget) + "\n")
    receipt["active_budget_seconds"] = budget["active_seconds"]
    receipt["finished_utc"] = datetime.now(timezone.utc).isoformat()
    (root / "logs" / f"{run_id}.json").write_text(
        json.dumps(receipt, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(receipt), flush=True)
    return outcome.returncode


if __name__ == "__main__":
    sys.exit(main())
