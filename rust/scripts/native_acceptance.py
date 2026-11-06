# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded offline acceptance of explicitly held, native Rust engineering binaries.

This development-only tool neither builds nor downloads code. Controller-pinned
host, lease, source and build records are prerequisites, not self-issued proof of
isolation. Receipts never constitute signing, live-policy or release acceptance.
"""

from __future__ import annotations

import argparse
import contextlib
import ctypes
import errno
import hashlib
import json
import math
import os
import platform
import re
import signal
import stat
import struct
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, Iterator

LIMIT = 64 * 1024
JSON_DEPTH_LIMIT = 64
MUTEX = r"Local\FabricRustParityValidation"
COMBINED = ["headless", "native-read", "repl", "self-update"]
TARGETS = {
    ("Windows", "x86_64"): "x86_64-pc-windows-msvc",
    ("Windows", "aarch64"): "aarch64-pc-windows-msvc",
    ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
    ("Linux", "aarch64"): "aarch64-unknown-linux-gnu",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Darwin", "aarch64"): "aarch64-apple-darwin",
}
ROLES = {
    "default": ("fabric-cli", "fab-rs", []),
    "optional-combined": ("fabric-cli", "fab-rs", COMBINED),
    "headless-tests": ("fabric-identity", "headless", ["headless"]),
    "updater-tests": ("fabric-cli-update", "contracts", ["running-replacement"]),
    "bounded-helper-tests": ("fabric-cli-update", "native_fixture", ["fixture-harness"]),
}
HELPER_FIXTURES = {
    "fabric-update-lifecycle-fixture",
    "fabric-update-helper-fixture",
}
NATIVE_TESTS = {
    "headless-tests": [
        "fifo_assertions_fail_in_an_isolated_process_with_bounded_shutdown",
        "aks_leaf_and_data_links_rotate_beneath_the_pinned_root",
        "redirected_parents_are_rejected_and_retargeting_cannot_switch_the_root",
    ],
    "updater-tests": [
        "real_non_running_fixture_replacement_retains_rollback_and_rejects_replay",
        "real_replacement_failure_retains_current_file_and_rollback",
        "existing_lock_stage_or_rollback_never_overwrites_or_deletes_other_files",
        "cancelled_install_does_not_stage_or_replace",
    ],
    "bounded-helper-tests": [
        "two_successive_upgrades_advance_receipt_and_reconcile_owned_state",
        "postready_late_error_keeps_outer_guard_alive_until_main_finishes",
        "receipt_failure_after_binary_commit_is_reconciled_without_replacement_replay",
        "rename_failure_and_helper_death_restore_the_exact_runnable_path_on_reconcile",
        "unknown_lock_and_package_manager_custody_and_unsigned_requests_never_mutate",
        "active_helper_is_pending_and_major_upgrades_require_explicit_permission",
        "postready_cancellation_and_asset_tampering_never_apply_or_delete_unknown_bytes",
        "independent_helper_rejects_tampered_signature_and_preserves_foreign_journal_files",
        "parent_lifetime_timeout_never_applies_and_can_reconcile_its_exact_stage",
    ],
}
INVALID = {
    "error": {
        "code": "invalid_arguments",
        "message": "Invalid arguments. Run fab-rs --help for available commands and usage.",
    }
}
REDIRECTED = {
    "error": {
        "code": "InteractiveInputUnavailable",
        "message": "Interactive mode requires terminal stdin and stdout.",
    }
}


class Rejected(ValueError):
    """A fixed diagnostic code safe to retain without untrusted input."""


def require(condition: bool, code: str) -> None:
    """Fail closed with a fixed diagnostic."""
    if not condition:
        raise Rejected(code)


def digest(raw: bytes) -> str:
    """Return a lowercase SHA-256 digest."""
    return hashlib.sha256(raw).hexdigest()


def canonical(value: Any) -> bytes:
    """Encode controller pins consistently, without a trailing newline."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()


def closed(value: Any, keys: set[str]) -> dict[str, Any]:
    """Reject missing and unknown fields rather than guessing a schema."""
    require(type(value) is dict and value.keys() == keys, "unknown_or_missing_fields")
    return value


def pairs(values: list[tuple[str, Any]]) -> dict[str, Any]:
    """Reject duplicate JSON object keys."""
    result: dict[str, Any] = {}
    for key, value in values:
        require(key not in result, "duplicate_key")
        result[key] = value
    return result


def parse(raw: bytes) -> Any:
    """Decode bounded strict JSON without exposing rejected data."""
    require(len(raw) <= LIMIT, "json_limit")

    def finite(value: str) -> float:
        result = float(value)
        require(math.isfinite(result), "json_constant")
        return result

    try:
        result = json.loads(
            raw, object_pairs_hook=pairs,
            parse_float=finite,
            parse_constant=lambda _: (_ for _ in ()).throw(Rejected("json_constant")),
        )
        pending = [(result, 0)]
        while pending:
            value, depth = pending.pop()
            if type(value) in (dict, list):
                require(depth < JSON_DEPTH_LIMIT, "json_depth_limit")
                children = value.values() if type(value) is dict else value
                pending.extend((child, depth + 1) for child in children)
        return result
    except (UnicodeError, ValueError, RecursionError) as error:
        if isinstance(error, Rejected):
            raise
        raise Rejected("invalid_json") from error


def absolute(value: str, *, exists: bool = True) -> Path:
    """Require absolute, link/reparse-free paths including all parent components."""
    require(type(value) is str and Path(value).is_absolute(), "absolute_path_required")
    path = Path(value)
    for part in [path, *path.parents]:
        if part.exists() or part.is_symlink():
            info = part.lstat()
            require(
                not stat.S_ISLNK(info.st_mode)
                and not getattr(info, "st_file_attributes", 0) & 0x400,
                "linked_path",
            )
    require(not exists or path.exists(), "missing_path")
    return path.resolve(strict=exists)


def file_hash(path: Path, deadline: float) -> str:
    """Hash a regular single-link file within the run deadline."""
    require(time.monotonic() < deadline, "whole_run_deadline")
    info = path.stat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, "not_exclusive_regular_file")
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(LIMIT):
            require(time.monotonic() < deadline, "whole_run_deadline")
            hasher.update(chunk)
    require(time.monotonic() < deadline, "whole_run_deadline")
    return hasher.hexdigest()


def json_bytes(path: Path, deadline: float) -> bytes:
    """Read at most the JSON bound, including files that grow after inspection."""
    require(time.monotonic() < deadline, "whole_run_deadline")
    info = path.stat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, "not_exclusive_regular_file")
    require(info.st_size <= LIMIT, "json_limit")
    with path.open("rb") as stream:
        raw = stream.read(LIMIT + 1)
    require(time.monotonic() < deadline, "whole_run_deadline")
    require(len(raw) <= LIMIT, "json_limit")
    return raw


def pinned_json(path: Path, expected: str, deadline: float) -> dict[str, Any]:
    """Read one externally pinned local controller record."""
    require(bool(re.fullmatch(r"[a-f0-9]{64}", expected)), "invalid_digest")
    raw = json_bytes(path, deadline)
    require(digest(raw) == expected, "record_hash_drift")
    return parse(raw)


def verify_tools(git: Path, git_sha256: str, python_sha256: str, deadline: float) -> dict[str, str]:
    """Bind the explicit Git tool and running interpreter to independent pins."""
    interpreter = absolute(str(Path(sys.executable).resolve(strict=True)))
    for path, expected in ((git, git_sha256), (interpreter, python_sha256)):
        require(type(expected) is str and bool(re.fullmatch(r"[a-f0-9]{64}", expected)),
                "invalid_tool_digest")
        require(file_hash(path, deadline) == expected, "tool_hash_drift")
    return {
        "git": str(git), "git_sha256": git_sha256,
        "interpreter": str(interpreter), "interpreter_sha256": python_sha256,
    }


def native_identity() -> tuple[str, str, str]:
    """Identify the actual interpreter OS/architecture, not a cross target."""
    machine = platform.machine().lower()
    arch = {"amd64": "x86_64", "arm64": "aarch64"}.get(machine, machine)
    system = platform.system()
    require((system, arch) in TARGETS, "unsupported_native_host")
    require(struct.calcsize("P") == 8, "wrong_interpreter_architecture")
    if system == "Windows":
        from ctypes import wintypes as w
        api = ctypes.WinDLL("kernel32", use_last_error=True)
        api.GetCurrentProcess.restype = w.HANDLE
        api.IsWow64Process2.argtypes = [w.HANDLE, ctypes.POINTER(w.USHORT), ctypes.POINTER(w.USHORT)]
        process_machine, native_machine = w.USHORT(), w.USHORT()
        require(bool(api.IsWow64Process2(api.GetCurrentProcess(), ctypes.byref(process_machine),
                                        ctypes.byref(native_machine)))
                and process_machine.value == 0
                and native_machine.value == (0xAA64 if arch == "aarch64" else 0x8664),
                "emulated_interpreter_not_native_proof")
    elif system == "Darwin":
        library = ctypes.CDLL(None, use_errno=True)
        translated, size = ctypes.c_int(), ctypes.c_size_t(ctypes.sizeof(ctypes.c_int))
        library.sysctlbyname.argtypes = [
            ctypes.c_char_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t),
            ctypes.c_void_p, ctypes.c_size_t,
        ]
        result = library.sysctlbyname(b"sysctl.proc_translated", ctypes.byref(translated),
                                     ctypes.byref(size), None, 0)
        require(result != 0 or translated.value == 0, "emulated_interpreter_not_native_proof")
    else:
        with Path(sys.executable).open("rb") as stream:
            header = stream.read(20)
        require(header[:6] == b"\x7fELF\x02\x01"
                and struct.unpack_from("<H", header, 18)[0] == (183 if arch == "aarch64" else 62),
                "emulated_interpreter_not_native_proof")
    return system, arch, TARGETS[(system, arch)]


def environment(root: Path) -> dict[str, str]:
    """Construct an allowlist; never enumerate or forward credential variables."""
    result = {
        "HOME": str(root), "USERPROFILE": str(root),
        "XDG_CONFIG_HOME": str(root), "XDG_CACHE_HOME": str(root),
        "TEMP": str(root), "TMP": str(root), "TMPDIR": str(root),
        "FAB_RUST_CONFIG_ROOT": str(root / "config"),
        "FAB_UPDATE_TEST_ROOT": str(root), "FAB_UPDATE_FIXTURE_ROOT": str(root),
        "CI": "1", "NO_COLOR": "1", "TERM": "xterm", "LANG": "C.UTF-8",
        "PATH": "", "GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull, "PYTHONDONTWRITEBYTECODE": "1",
    }
    if os.name == "nt":
        system = Path(os.environ["SystemRoot"])
        result.update(SystemRoot=str(system), WINDIR=str(system))
    return result


class WindowsTree:
    """Place a suspended owned PID in a kill-on-close Job before resuming it."""

    def __init__(self) -> None:
        from ctypes import wintypes as w

        class Basic(ctypes.Structure):
            _fields_ = [
                ("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64),
                ("flags", w.DWORD), ("minimum", ctypes.c_size_t),
                ("maximum", ctypes.c_size_t), ("active_limit", w.DWORD),
                ("affinity", ctypes.c_size_t), ("priority", w.DWORD),
                ("scheduling", w.DWORD),
            ]

        class IO(ctypes.Structure):
            _fields_ = [(name, ctypes.c_uint64) for name in
                        ("read_ops", "write_ops", "other_ops", "read", "write", "other")]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", IO)] + [
                (name, ctypes.c_size_t) for name in
                ("process_memory", "job_memory", "peak_process", "peak_job")
            ]

        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        signatures = {
            "CreateJobObjectW": ([ctypes.c_void_p, w.LPCWSTR], w.HANDLE),
            "SetInformationJobObject": ([w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD], w.BOOL),
            "AssignProcessToJobObject": ([w.HANDLE, w.HANDLE], w.BOOL),
            "TerminateJobObject": ([w.HANDLE, w.UINT], w.BOOL),
            "QueryInformationJobObject": ([w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD, ctypes.c_void_p], w.BOOL),
            "CloseHandle": ([w.HANDLE], w.BOOL),
            "CreateToolhelp32Snapshot": ([w.DWORD, w.DWORD], w.HANDLE),
            "Thread32First": ([w.HANDLE, ctypes.c_void_p], w.BOOL),
            "Thread32Next": ([w.HANDLE, ctypes.c_void_p], w.BOOL),
            "OpenThread": ([w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            "ResumeThread": ([w.HANDLE], w.DWORD),
        }
        for name, (args, returns) in signatures.items():
            method = getattr(self.api, name)
            method.argtypes, method.restype = args, returns
        self.handle = self.api.CreateJobObjectW(None, None)
        require(bool(self.handle), "job_create_failed")
        limits = Extended()
        limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not self.api.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)):
            self.api.CloseHandle(self.handle)
            raise Rejected("job_limits_failed")

    def attach(self, child: subprocess.Popen[bytes], deadline: float) -> None:
        """Assign the not-yet-running process, then resume only its primary thread."""
        from ctypes import wintypes as w

        require(bool(self.api.AssignProcessToJobObject(self.handle, int(child._handle))), "job_assign_failed")

        class Thread(ctypes.Structure):
            _fields_ = [
                ("size", w.DWORD), ("usage", w.DWORD), ("tid", w.DWORD),
                ("pid", w.DWORD), ("base", w.LONG), ("delta", w.LONG), ("flags", w.DWORD),
            ]

        snapshot = self.api.CreateToolhelp32Snapshot(4, 0)
        require(snapshot not in (None, ctypes.c_void_p(-1).value), "thread_snapshot_failed")
        tids = []
        try:
            entry = Thread()
            entry.size = ctypes.sizeof(entry)
            present = self.api.Thread32First(snapshot, ctypes.byref(entry))
            while present:
                require(time.monotonic() < deadline, "process_start_deadline")
                if entry.pid == child.pid:
                    tids.append(entry.tid)
                present = self.api.Thread32Next(snapshot, ctypes.byref(entry))
        finally:
            self.api.CloseHandle(snapshot)
        require(len(tids) == 1, "primary_thread_not_unique")
        require(time.monotonic() < deadline, "process_start_deadline")
        thread = self.api.OpenThread(2, False, tids[0])
        require(bool(thread), "primary_thread_open_failed")
        try:
            require(self.api.ResumeThread(thread) == 1, "primary_thread_resume_failed")
        finally:
            self.api.CloseHandle(thread)

    def close(self) -> bool:
        """Terminate only this Job's tree and observe zero active processes."""
        # JOBOBJECT_BASIC_ACCOUNTING_INFORMATION: four LARGE_INTEGERs, four DWORDs.
        info = (ctypes.c_byte * 48)()
        self.api.TerminateJobObject(self.handle, 125)
        end = time.monotonic() + 1
        empty = False
        while time.monotonic() < end:
            if self.api.QueryInformationJobObject(self.handle, 1, info, 48, None):
                empty = struct.unpack_from("I", bytes(info), 40)[0] == 0
                if empty:
                    break
            time.sleep(0.01)
        self.api.CloseHandle(self.handle)
        return empty


def wait_owned_process(child: subprocess.Popen[bytes], timeout: float, master: int | None) -> int:
    """Reap the owned child while draining a PTY within the existing wait budget."""
    if master is None:
        return child.wait(timeout=timeout)
    import select

    deadline = time.monotonic() + timeout
    while True:
        code = child.poll()
        if code is not None:
            return code
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(child.args, timeout)
        try:
            readable, _, _ = select.select([master], [], [], min(0.01, remaining))
        except ValueError as error:
            raise OSError(errno.EINVAL, "Owned PTY selector failed") from error
        if readable:
            # A full Darwin PTY can block the killed session leader's final close.
            # Capture has already ended; discard queued bytes, never extend its limit.
            try:
                chunk = os.read(master, 4096)
            except BlockingIOError:
                continue
            except OSError as error:
                if error.errno != errno.EIO:
                    raise
                chunk = b""
            if not chunk:
                time.sleep(min(0.005, max(0, deadline - time.monotonic())))


def execute(
    argv: list[str], cwd: Path, env: dict[str, str], seconds: float,
    deadline: float, stdin: bytes = b"", terminal: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], bytes, bytes]:
    """Execute one explicit argv with bounded capture, deadlines and owned-tree cleanup."""
    started = time.monotonic()
    started_utc = datetime.now(timezone.utc).isoformat()
    end = min(started + seconds, deadline)
    require(end > started, "whole_run_deadline")
    require(terminal is None or os.name != "nt", "native_PTY_unavailable")
    tree = WindowsTree() if os.name == "nt" else None
    child: subprocess.Popen[bytes] | None = None
    buffers = [bytearray(), bytearray()]
    overflow = threading.Event()
    threads: list[threading.Thread] = []
    master = slave = None
    restored = raw_observed = False
    input_queue_limit = False
    timed_out = False
    execution_error: str | None = None
    cleanup_failures: list[str] = []
    before: Any = None

    def capture(stream: Any, buffer: bytearray) -> None:
        try:
            while chunk := stream.read(4096):
                if len(buffer) + len(chunk) > LIMIT:
                    overflow.set()
                buffer.extend(chunk[: max(0, LIMIT - len(buffer))])
        except (OSError, ValueError):
            overflow.set()

    try:
        options: dict[str, Any] = {
            "cwd": cwd, "env": env, "shell": False,
            "stdin": subprocess.PIPE, "stdout": subprocess.PIPE, "stderr": subprocess.PIPE,
        }
        if os.name == "nt":
            options["creationflags"] = 0x4 | subprocess.CREATE_NEW_PROCESS_GROUP
        elif terminal is None:
            options["start_new_session"] = True
        else:
            import fcntl
            import pty
            import termios

            master, slave = pty.openpty()
            os.set_blocking(master, False)
            before = termios.tcgetattr(slave)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))

            def controlling_terminal() -> None:
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)

            options.update(stdin=slave, stdout=slave, stderr=slave, preexec_fn=controlling_terminal)
        child = subprocess.Popen(argv, **options)
        require(time.monotonic() < end, "process_start_deadline")
        if tree:
            tree.attach(child, end)
        if terminal is None:
            for stream, buffer in zip((child.stdout, child.stderr), buffers):
                thread = threading.Thread(target=capture, args=(stream, buffer), daemon=True)
                thread.start()
                threads.append(thread)
            require(len(stdin) <= LIMIT, "stdin_limit")

            def feed() -> None:
                try:
                    child.stdin.write(stdin)
                    child.stdin.close()
                except (BrokenPipeError, OSError, ValueError):
                    pass

            thread = threading.Thread(target=feed, daemon=True)
            thread.start()
            threads.append(thread)
        else:
            import select
            import termios

            sent = False
            interrupted = False
            prompt = b"$ "
            dsr = 0
            input_bytes_written = 0
            query_tail = b""
            pending = bytearray()

            def enqueue(data: bytes) -> None:
                nonlocal input_queue_limit
                if len(pending) + len(data) > LIMIT:
                    input_queue_limit = True
                else:
                    pending.extend(data)

            while (child.poll() is None and time.monotonic() < end
                   and not overflow.is_set() and not input_queue_limit):
                flags = termios.tcgetattr(slave)[3]
                raw_observed |= not bool(flags & (termios.ECHO | termios.ICANON))
                readable, writable, _ = select.select(
                    [master], [master] if pending else [], [],
                    min(0.01, max(0, end - time.monotonic())),
                )
                if time.monotonic() >= end:
                    break
                if readable:
                    try:
                        chunk = os.read(master, 4096)
                    except BlockingIOError:
                        chunk = b""
                    except OSError:
                        break
                    if len(buffers[0]) + len(chunk) > LIMIT:
                        overflow.set()
                    buffers[0].extend(chunk[: max(0, LIMIT - len(buffers[0]))])
                    # PTY protocol only; this does not verify terminal rendering/UX.
                    queries = (query_tail + chunk).count(b"\x1b[6n")
                    query_tail = (query_tail + chunk)[-3:]
                    dsr += queries
                    enqueue(b"\x1b[1;1R" * queries)
                    if raw_observed and prompt in buffers[0] and not sent:
                        enqueue(terminal["input"].encode("ascii"))
                        sent = True
                    if terminal["name"] == "interrupt" and sent and not interrupted:
                        if b"Use 'quit' or 'exit'" in buffers[0]:
                            enqueue(b"quit\r")
                            interrupted = True
                if (writable and pending and not overflow.is_set() and not input_queue_limit
                        and time.monotonic() < end):
                    try:
                        written = os.write(master, pending[:4096])
                    except BlockingIOError:
                        pass
                    except OSError:
                        break
                    else:
                        input_bytes_written += written
                        del pending[:written]
            terminal["protocol_cursor_queries"] = dsr
            terminal["protocol_input_bytes_written"] = input_bytes_written
            while (time.monotonic() < end and not overflow.is_set()
                   and select.select([master], [], [], 0)[0]):
                try:
                    chunk = os.read(master, 4096)
                except BlockingIOError:
                    break
                except OSError:
                    break
                if not chunk:
                    break
                if len(buffers[0]) + len(chunk) > LIMIT:
                    overflow.set()
                buffers[0].extend(chunk[: max(0, LIMIT - len(buffers[0]))])
        while (child.poll() is None and time.monotonic() < end
               and not overflow.is_set() and not input_queue_limit):
            time.sleep(0.005)
        timed_out = (time.monotonic() >= end or child.poll() is None) and not overflow.is_set() and not input_queue_limit
    except (Rejected, OSError, ValueError) as error:
        execution_error = str(error) if isinstance(error, Rejected) else "process_execution_failure"
        timed_out |= time.monotonic() >= end
    finally:
        if tree:
            cleanup = tree.close()
            if not cleanup:
                cleanup_failures.append("job_members_unconfirmed")
        elif child is not None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError:
                cleanup_failures.append("process_group_signal_failed")
            cleanup = not cleanup_failures
        else:
            cleanup = True
        if child is not None:
            try:
                wait_owned_process(child, 1, master)
            except subprocess.TimeoutExpired:
                try:
                    child.kill()  # A specific owned PID; never a process-name kill.
                except OSError:
                    cleanup_failures.append("owned_process_signal_failed")
                cleanup = False
                try:
                    wait_owned_process(child, 0.5, master)
                except (subprocess.TimeoutExpired, OSError):
                    cleanup_failures.append("owned_process_wait_unconfirmed")
            except OSError:
                cleanup = False
                cleanup_failures.append("owned_process_wait_unconfirmed")
            if os.name != "nt":
                group_deadline = time.monotonic() + 0.3
                while True:
                    try:
                        os.killpg(child.pid, 0)
                    except ProcessLookupError:
                        break
                    except OSError:
                        cleanup = False
                        cleanup_failures.append("process_group_probe_failed")
                        break
                    if time.monotonic() >= group_deadline:
                        cleanup = False
                        cleanup_failures.append("process_group_still_present")
                        break
                    time.sleep(0.01)
            for thread in threads:
                thread.join(0.2)
            if any(thread.is_alive() for thread in threads):
                cleanup = False
                cleanup_failures.append("io_threads_still_active")
        if slave is not None:
            import termios
            try:
                restored = termios.tcgetattr(slave) == before
            except (OSError, termios.error):
                cleanup = False
                cleanup_failures.append("terminal_state_unavailable")
            for descriptor in (master, slave):
                try:
                    os.close(descriptor)
                except OSError:
                    cleanup = False
                    cleanup_failures.append("terminal_descriptor_close_failed")
        if child is not None:
            for stream in (child.stdin, child.stdout, child.stderr):
                # Never wait on a buffered-reader lock after unconfirmed cleanup.
                if stream and not any(thread.is_alive() for thread in threads):
                    try:
                        stream.close()
                    except (OSError, ValueError):
                        cleanup = False
                        cleanup_failures.append("pipe_close_failed")
    out, err = bytes(buffers[0]), bytes(buffers[1])
    receipt = {
        "argv": argv, "pid": child.pid if child is not None else None,
        "exit": child.returncode if child is not None else None,
        "cwd": str(cwd), "environment_keys": sorted(env),
        "started_utc": started_utc, "finished_utc": datetime.now(timezone.utc).isoformat(),
        "command_budget_seconds": seconds,
        "effective_budget_seconds": round(end - started, 4),
        "stdin_bytes": len(stdin), "stdin_sha256": digest(stdin),
        "timeout": timed_out, "output_limit": overflow.is_set(),
        "cleanup": ("no_child_created" if child is None else
                    "owned_tree_terminated" if cleanup else "unconfirmed"),
        "cleanup_failure_codes": cleanup_failures,
        "elapsed_seconds": round(time.monotonic() - started, 4),
        "stdout_bytes": len(out), "stderr_bytes": len(err),
        "stdout_sha256": digest(out), "stderr_sha256": digest(err),
    }
    if execution_error is not None:
        receipt["execution_error"] = execution_error
    if terminal is not None:
        receipt["terminal"] = {
            "kind": "native_kernel_pty_not_visual_acceptance",
            "raw_observed": raw_observed, "termios_restored": restored,
            "cursor_queries": terminal.get("protocol_cursor_queries", 0),
            "input_bytes_written": terminal.get("protocol_input_bytes_written", 0),
            "input_queue_limit": input_queue_limit,
        }
    return receipt, out, err


def source_snapshot(root: Path, git: Path, deadline: float, scratch: Path) -> dict[str, Any]:
    """Record HEAD and every dirty/untracked file without running source or hooks."""
    root = absolute(str(root))

    def read(*args: str) -> bytes:
        receipt, out, err = execute(
            [str(git), "--no-pager", "-c", "core.fsmonitor=false",
             "-c", "status.renames=false", *args],
            root, environment(scratch), 5, deadline,
        )
        require(receipt["exit"] == 0 and not receipt["timeout"]
                and not receipt["output_limit"] and receipt["cleanup"] != "unconfirmed"
                and not receipt.get("execution_error") and not err,
                "source_inspection_failed")
        return out

    actual_root = absolute(read("rev-parse", "--show-toplevel").decode("utf-8").rstrip("\r\n"))
    require(root == actual_root, "candidate_root_not_repository_top_level")
    head = read("rev-parse", "--verify", "HEAD").decode("ascii").strip()
    require(bool(re.fullmatch(r"[a-f0-9]{40}", head)), "invalid_source_commit")
    status_raw = read("status", "--porcelain=v1", "-z", "--untracked-files=all", "--ignore-submodules=none")
    files = []
    for row in status_raw.split(b"\0"):
        if not row:
            continue
        require(len(row) > 3 and row[2:3] == b" ", "unsupported_git_status")
        state = row[:2].decode("ascii")
        name = row[3:].decode("utf-8")
        relative = PurePosixPath(name)
        require(not relative.is_absolute() and ".." not in relative.parts
                and "\\" not in name and ":" not in name, "unsafe_source_path")
        path = absolute(str(root.joinpath(*relative.parts)), exists=False)
        require(path.is_relative_to(root), "source_path_escape")
        exists = path.exists()
        deletion = state in {" D", "D ", "MD", "AD", "RD", "CD", "TD", "DD", "UD", "DU"}
        require(exists or deletion, "missing_non_deleted_source_file")
        require(not exists or path.is_file(), "unsupported_dirty_entry")
        files.append({
            "path": name, "status": state,
            "sha256": file_hash(path, deadline) if exists else None,
        })
    return {"head": head, "status_sha256": digest(status_raw), "dirty_files": files}


def validate_request(request: dict[str, Any], target: str) -> None:
    """Validate closed schemas and exact feature/build custody before any product runs."""
    closed(request, {
        "schema", "run_id", "candidate_root", "git", "git_sha256", "python_sha256",
        "source", "version",
        "cargo_metadata", "cargo_metadata_sha256", "binaries",
        "per_command_seconds", "whole_run_seconds",
    })
    require(type(request["schema"]) is int and request["schema"] == 1, "unknown_request_schema")
    for key in ("git_sha256", "python_sha256"):
        require(type(request[key]) is str and bool(re.fullmatch(r"[a-f0-9]{64}", request[key])),
                "invalid_tool_digest")
    require(bool(re.fullmatch(r"[a-z0-9-]{1,48}", request["run_id"])), "invalid_run_id")
    require(type(request["per_command_seconds"]) is int and 1 <= request["per_command_seconds"] <= 60,
            "invalid_command_deadline")
    require(type(request["whole_run_seconds"]) is int and 1 <= request["whole_run_seconds"] <= 600,
            "invalid_run_deadline")
    require(bool(re.fullmatch(r"\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?", request["version"])), "invalid_version")
    require(type(request["binaries"]) is list and len(request["binaries"]) <= len(ROLES), "invalid_binaries")
    closed(request["source"], {"head", "status_sha256", "dirty_files"})
    require(bool(re.fullmatch(r"[a-f0-9]{40}", request["source"]["head"]))
            and bool(re.fullmatch(r"[a-f0-9]{64}", request["source"]["status_sha256"]))
            and type(request["source"]["dirty_files"]) is list, "invalid_source_snapshot")
    for item in request["source"]["dirty_files"]:
        closed(item, {"path", "status", "sha256"})
        require(type(item["path"]) is str and type(item["status"]) is str
                and len(item["status"]) == 2
                and (item["sha256"] is None or bool(re.fullmatch(r"[a-f0-9]{64}", item["sha256"]))),
                "invalid_dirty_manifest")
    seen = set()
    for binary in request["binaries"]:
        closed(binary, {
            "role", "path", "sha256", "bytes", "package", "target_name", "target",
            "features", "resolved_features", "default_features", "source_snapshot_sha256",
            "rustc", "cargo", "build_command", "custody_ref", "auxiliary_binaries",
        })
        role = binary["role"]
        require(role in ROLES and role not in seen, "unknown_or_duplicate_binary_role")
        seen.add(role)
        package, name, features = ROLES[role]
        require(binary["target"] == target, "wrong_platform")
        require(binary["package"] == package and binary["target_name"] == name, "wrong_binary")
        require(binary["features"] == features and binary["default_features"] is False, "missing_or_wrong_feature")
        require(type(binary["resolved_features"]) is list
                and all(type(f) is str for f in binary["resolved_features"])
                and binary["resolved_features"] == sorted(set(binary["resolved_features"])),
                "invalid_resolved_features")
        require(binary["source_snapshot_sha256"] == digest(canonical(request["source"])), "candidate_hash_drift")
        require(type(binary["bytes"]) is int and 0 < binary["bytes"] <= 512 * 1024**2, "binary_size")
        for key in ("sha256", "source_snapshot_sha256"):
            require(bool(re.fullmatch(r"[a-f0-9]{64}", binary[key])), "invalid_digest")
        for key in ("rustc", "cargo", "custody_ref"):
            require(type(binary[key]) is str and 0 < len(binary[key]) <= 512
                    and not any(ord(c) < 32 for c in binary[key]), "missing_build_custody")
        require(type(binary["build_command"]) is list and 0 < len(binary["build_command"]) <= 64
                and all(type(a) is str and len(a) <= 1024 for a in binary["build_command"]),
                "missing_build_command")
        require(type(binary["auxiliary_binaries"]) is list and len(binary["auxiliary_binaries"]) <= 4,
                "invalid_auxiliary_binaries")
        for item in binary["auxiliary_binaries"]:
            keys = {"path", "sha256", "bytes"}
            if role == "bounded-helper-tests":
                keys.add("target_name")
            closed(item, keys)
            require(type(item["bytes"]) is int and 0 < item["bytes"] <= 512 * 1024**2
                    and bool(re.fullmatch(r"[a-f0-9]{64}", item["sha256"])),
                    "invalid_auxiliary_binary_pin")
        if role == "bounded-helper-tests":
            auxiliaries = binary["auxiliary_binaries"]
            require(len(auxiliaries) == 2
                    and {item["target_name"] for item in auxiliaries} == HELPER_FIXTURES
                    and len({item["path"] for item in auxiliaries}) == 2
                    and all(item["path"] != binary["path"] for item in auxiliaries),
                    "both_helper_fixture_artifacts_required")
            require(request["per_command_seconds"] == 60 and request["whole_run_seconds"] >= 180,
                    "helper_lifecycle_budget_required")


def validate_output(kind: str, out: bytes, err: bytes, version: str) -> None:
    """Check only explicit output contracts; unknown schemas cannot succeed."""
    if kind == "help":
        require(not err and all(value in out for value in (b"Usage: fab-rs", b"--help", b"--version")),
                "help_contract")
    elif kind == "version":
        require(not err and out == f"fab-rs {version}\n".encode(), "version_contract")
    elif kind in ("invalid", "redirected"):
        require(not out and err.endswith(b"\n"), "stream_contract")
        require(parse(err) == (INVALID if kind == "invalid" else REDIRECTED), "error_schema")
    else:
        raise Rejected("unknown_output_schema")


def validate_metadata(metadata: Any) -> None:
    """Reject malformed Cargo projections before traversing their feature graph."""
    closed(metadata, {"version", "packages", "workspace_members"})
    require(type(metadata["version"]) is int and metadata["version"] == 1
            and type(metadata["packages"]) is list
            and type(metadata["workspace_members"]) is list
            and all(type(member) is str for member in metadata["workspace_members"]),
            "unknown_cargo_metadata")
    for package in metadata["packages"]:
        require(type(package) is dict and {"id", "name", "features", "targets"} <= package.keys(),
                "unknown_cargo_metadata")
        require(type(package["id"]) is str and type(package["name"]) is str
                and type(package["features"]) is dict and type(package["targets"]) is list,
                "unknown_cargo_metadata")
        require(all(type(name) is str and type(edges) is list
                    and all(type(edge) is str for edge in edges)
                    for name, edges in package["features"].items()), "unknown_cargo_metadata")
        require(all(type(target) is dict and type(target.get("name")) is str
                    and type(target.get("kind")) is list
                    and all(type(kind) is str for kind in target["kind"])
                    for target in package["targets"]), "unknown_cargo_metadata")


def exact_test_passed(receipt: dict[str, Any], out: bytes, err: bytes, name: str) -> bool:
    """Reject libtest's successful zero-test/ignored/filtered-only outcomes."""
    lines = out.splitlines()
    results = [line for line in lines if line.startswith(b"test result:")]
    tests = [line for line in lines if line.startswith(b"test ") and not line.startswith(b"test result:")]
    return (
        receipt["exit"] == 0 and not receipt["timeout"] and not receipt["output_limit"]
        and receipt["cleanup"] != "unconfirmed" and not receipt.get("execution_error") and not err
        and tests == [(f"test {name} ... ok").encode()]
        and len(results) == 1
        and re.fullmatch(
            rb"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; "
            rb"[0-9]+ filtered out; finished in [0-9]+(?:\.[0-9]+)?s",
            results[0],
        ) is not None
    )


@contextlib.contextmanager
def controller_slot(root: Path) -> Iterator[None]:
    """Acquire the established shared-host mutex/lock without waiting or stealing."""
    if os.name == "nt":
        from ctypes import wintypes as w
        api = ctypes.WinDLL("kernel32", use_last_error=True)
        api.CreateMutexW.argtypes = [ctypes.c_void_p, w.BOOL, w.LPCWSTR]
        api.CreateMutexW.restype = w.HANDLE
        api.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
        api.ReleaseMutex.argtypes = [w.HANDLE]
        api.CloseHandle.argtypes = [w.HANDLE]
        handle = api.CreateMutexW(None, False, MUTEX)
        require(bool(handle), "mutex_create_failed")
        acquired = False
        try:
            outcome = api.WaitForSingleObject(handle, 0)
            acquired = outcome in (0, 0x80)
            require(outcome == 0, "controller_slot_busy_or_abandoned")
            yield
        finally:
            if acquired:
                api.ReleaseMutex(handle)
            api.CloseHandle(handle)
    else:
        import fcntl
        with (root / "native-acceptance.lock").open("a") as stream:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise Rejected("controller_slot_busy") from error
            try:
                yield
            finally:
                fcntl.flock(stream, fcntl.LOCK_UN)


def verify_binary(binary: dict[str, Any], deadline: float) -> None:
    """Verify every explicitly named executable, including native-test auxiliaries."""
    seen: set[Path] = set()
    for item in [binary, *binary["auxiliary_binaries"]]:
        path = absolute(item["path"])
        require(path not in seen, "duplicate_binary_path")
        seen.add(path)
        require(path.stat().st_size == item["bytes"]
                and file_hash(path, deadline) == item["sha256"], "wrong_binary_digest")
        with path.open("rb") as stream:
            header = stream.read(4096)
        target = binary["target"]
        arm = target.startswith("aarch64")
        if "windows" in target:
            require(header[:2] == b"MZ" and len(header) >= 64, "wrong_binary_format")
            offset = struct.unpack_from("<I", header, 60)[0]
            require(offset + 24 <= len(header) and header[offset:offset + 4] == b"PE\0\0"
                    and struct.unpack_from("<H", header, offset + 4)[0] == (0xAA64 if arm else 0x8664)
                    and not struct.unpack_from("<H", header, offset + 22)[0] & 0x2000,
                    "wrong_binary_architecture")
        elif "linux" in target:
            require(len(header) >= 20 and header[:6] == b"\x7fELF\x02\x01"
                    and struct.unpack_from("<H", header, 18)[0] == (183 if arm else 62),
                    "wrong_binary_architecture")
        else:
            require(len(header) >= 8 and header[:4] == b"\xcf\xfa\xed\xfe"
                    and struct.unpack_from("<I", header, 4)[0] == (0x100000C if arm else 0x1000007),
                    "wrong_binary_architecture")


def write_receipt(path: Path, value: dict[str, Any]) -> None:
    """Create, never overwrite, one metadata-only receipt."""
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=True)
        stream.write("\n")


def expected_cases() -> list[str]:
    """Keep aborted and missing-artifact coverage explicit in every final report."""
    return [
        f"{role}/{name}" for role in ("default", "optional-combined")
        for name in ("help", "short-help", "version", "short-version", "invalid", "scripted-stdin")
    ] + [
        f"repl/{name}" for name in ("quit", "eof", "lexical-error", "interrupt")
    ] + [
        f"{role}/{name}{suffix}" for role, names in NATIVE_TESTS.items()
        for name in names for suffix in ("/inventory", "")
    ]


def run(
    request: dict[str, Any], host: dict[str, Any], lease: dict[str, Any], root: Path,
    started: float,
) -> tuple[dict[str, Any], int]:
    """Run the exact local matrix; absent evidence always stays blocked/not_run."""
    system, arch, target = native_identity()
    validate_request(request, target)
    closed(host, {"schema", "registered_host_ref", "system", "architecture", "platform",
                  "native", "emulated", "provider_credentials_absent", "outbound_blocked",
                  "isolation_evidence_ref"})
    require(type(host["schema"]) is int and host["schema"] == 1
            and host["system"] == system and host["architecture"] == arch
            and host["platform"] == platform.platform(), "wrong_host_evidence")
    require(host["native"] is True and host["emulated"] is False
            and host["provider_credentials_absent"] is True and host["outbound_blocked"] is True,
            "native_host_or_isolation_not_approved")
    require(all(type(host[key]) is str and 0 < len(host[key]) <= 512
                for key in ("registered_host_ref", "isolation_evidence_ref")), "missing_host_evidence")
    closed(lease, {"schema", "controller_ref", "run_id", "artifact_root", "expires_utc",
                   "policy", "trusted_binary_execution"})
    require(type(lease["schema"]) is int and lease["schema"] == 1 and lease["run_id"] == request["run_id"]
            and lease["artifact_root"] == str(root)
            and lease["policy"] == MUTEX and lease["trusted_binary_execution"] is True
            and type(lease["controller_ref"]) is str and bool(lease["controller_ref"]),
            "controller_lease_required")
    expiry = datetime.fromisoformat(lease["expires_utc"])
    require(expiry.tzinfo is not None and
            (expiry - datetime.now(timezone.utc)).total_seconds() > request["whole_run_seconds"] + 5,
            "controller_lease_expired_or_too_short")
    deadline = started + request["whole_run_seconds"]
    source = absolute(request["candidate_root"])
    git = absolute(request["git"])
    require(not root.is_relative_to(source), "artifact_root_must_be_outside_candidate")
    directory = root / request["run_id"]
    directory.mkdir()  # Exclusive ownership; existing/partial runs are never reused.
    report: dict[str, Any] = {
        "schema": 1, "classification": "offline_engineering_only_not_release_acceptance",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "host": host, "lease": lease, "source": request["source"],
        "binaries": request["binaries"], "cases": [],
        "tool": {
            "python": sys.version, "interpreter": sys.executable,
        },
        "limitations": [
            "Controller records are externally reviewed attestations, not a sandbox.",
            "No clean-machine no-Python, visual terminal, live identity or publisher proof.",
            "Libtest success proves only the exact named offline contracts.",
        ],
    }
    cases = report["cases"]
    binaries = {item["role"]: item for item in request["binaries"]}

    def record(case: dict[str, Any]) -> None:
        cases.append(case)
        write_receipt(directory / f"case-{len(cases):03}.json", case)

    def command(role: str, case_id: str, args: list[str], *, stdin: bytes = b"",
                terminal: dict[str, Any] | None = None) -> tuple[dict[str, Any], bytes, bytes] | None:
        if role not in binaries:
            record({"id": case_id, "status": "not_run", "reason": "missing_binary", "role": role})
            return None
        if time.monotonic() >= deadline:
            record({"id": case_id, "status": "not_run", "reason": "whole_run_deadline", "role": role})
            return None
        command_deadline = min(deadline, time.monotonic() + request["per_command_seconds"])
        verify_binary(binaries[role], command_deadline)
        work = directory / f"work-{len(cases):03}"
        work.mkdir()
        result, out, err = execute(
            [binaries[role]["path"], *args], work, environment(work),
            request["per_command_seconds"], command_deadline, stdin, terminal,
        )
        result.update(id=case_id, role=role, status="failed")
        if result["cleanup"] == "unconfirmed":
            result["reason"] = "owned_cleanup_unconfirmed"
            record(result)
            raise Rejected("owned_cleanup_unconfirmed")
        if result.get("execution_error"):
            result["reason"] = result["execution_error"]
            record(result)
            raise Rejected("process_execution_failure")
        return result, out, err

    try:
        report["tool"]["script_sha256"] = file_hash(Path(__file__).resolve(), deadline)
        tools = verify_tools(git, request["git_sha256"], request["python_sha256"], deadline)
        report["tool"].update(tools)
        before = source_snapshot(source, git, deadline, directory)
        report["observed_source_before"] = before
        require(before == request["source"], "candidate_hash_drift")
        metadata_path = absolute(request["cargo_metadata"])
        metadata = pinned_json(metadata_path, request["cargo_metadata_sha256"], deadline)
        validate_metadata(metadata)
        for binary in request["binaries"]:
            packages = [p for p in metadata["packages"] if p.get("name") == binary["package"]]
            require(len(packages) == 1 and packages[0]["id"] in metadata["workspace_members"]
                    and all(f in packages[0].get("features", {}) for f in binary["features"])
                    and any(t.get("name") == binary["target_name"]
                            and ("bin" if binary["role"] in ("default", "optional-combined") else "test")
                            in t.get("kind", []) for t in packages[0].get("targets", [])),
                    "metadata_build_scope_mismatch")
            resolved = set(binary["features"])
            while True:
                require(time.monotonic() < deadline, "whole_run_deadline")
                expanded = resolved | {
                    edge for feature in resolved for edge in packages[0]["features"][feature]
                    if edge in packages[0]["features"]
                }
                if expanded == resolved:
                    break
                resolved = expanded
            require(binary["resolved_features"] == sorted(resolved), "resolved_feature_drift")
            if binary["role"] == "bounded-helper-tests":
                for auxiliary in binary["auxiliary_binaries"]:
                    require(any(
                        item.get("name") == auxiliary["target_name"] and "bin" in item.get("kind", [])
                        for item in packages[0]["targets"]
                    ), "helper_fixture_metadata_mismatch")
            verify_binary(binary, deadline)
        for role in ("default", "optional-combined"):
            for name, args, expected, code in [
                ("help", ["--help"], "help", 0),
                ("short-help", ["-h"], "help", 0),
                ("version", ["--version"], "version", 0),
                ("short-version", ["-V"], "version", 0),
                ("invalid", ["--client-secret", "must-not-be-echoed"], "invalid", 2),
                ("scripted-stdin", [], "redirected" if role == "optional-combined" else "invalid",
                 1 if role == "optional-combined" else 2),
            ]:
                result = command(role, f"{role}/{name}", args, stdin=b"quit\n" if not args else b"")
                if result is None:
                    continue
                receipt, out, err = result
                try:
                    require(receipt["exit"] == code and not receipt["timeout"]
                            and not receipt["output_limit"] and receipt["cleanup"] != "unconfirmed", "command_failed")
                    require(b"must-not-be-echoed" not in out + err, "redaction_failed")
                    validate_output(expected, out, err, request["version"])
                    receipt["status"] = "passed"
                except Rejected as error:
                    receipt["reason"] = str(error)
                record(receipt)
        fixture_path = Path(__file__).resolve().parents[1] / "tests" / "acceptance" / "native" / "terminal-inputs.json"
        fixture_raw = json_bytes(fixture_path, deadline)
        fixture = parse(fixture_raw)
        closed(fixture, {"schema", "cases"})
        require(type(fixture["schema"]) is int and fixture["schema"] == 1
                and type(fixture["cases"]) is list and len(fixture["cases"]) == 4,
                "unknown_terminal_fixture")
        for case in fixture["cases"]:
            closed(case, {"name", "input", "contains", "exit"})
            require(type(case["name"]) is str and type(case["input"]) is str
                    and len(case["input"]) <= 256 and case["input"].isascii()
                    and type(case["contains"]) is str and 0 < len(case["contains"]) <= 256
                    and type(case["exit"]) is int and case["exit"] == 0,
                    "unknown_terminal_fixture")
        require({case["name"] for case in fixture["cases"]} ==
                {"quit", "eof", "lexical-error", "interrupt"}, "unknown_terminal_fixture")
        report["terminal_fixture_sha256"] = digest(fixture_raw)
        for case in fixture["cases"]:
            closed(case, {"name", "input", "contains", "exit"})
            case_id = f"repl/{case['name']}"
            if system == "Windows":
                record({"id": case_id, "status": "not_run", "reason": "requires_native_PTY_or_manual_terminal"})
                continue
            result = command("optional-combined", case_id, [], terminal=dict(case))
            if result is None:
                continue
            receipt, out, _ = result
            terminal_result = receipt["terminal"]
            if (receipt["exit"] == case["exit"] and not receipt["timeout"]
                    and not receipt["output_limit"] and receipt["cleanup"] != "unconfirmed"
                    and terminal_result["raw_observed"] and terminal_result["termios_restored"]
                    and not terminal_result["input_queue_limit"]
                    and case["contains"].encode() in out):
                receipt["status"] = "passed"
            else:
                receipt["reason"] = "terminal_lifecycle_contract"
            record(receipt)
        for role, names in NATIVE_TESTS.items():
            for name in names:
                case_id = f"{role}/{name}"
                if role == "headless-tests" and system == "Windows":
                    record({"id": case_id, "status": "not_run", "reason": "requires_native_Unix_runtime"})
                    continue
                listing = command(role, f"{case_id}/inventory", ["--list", "--format", "terse"])
                if listing is None:
                    continue
                receipt, out, err = listing
                found = out.splitlines().count((name + ": test").encode()) == 1
                valid = (receipt["exit"] == 0 and not receipt["timeout"] and not err
                         and not receipt["output_limit"] and receipt["cleanup"] != "unconfirmed" and found)
                receipt["status"] = "passed" if valid else "failed"
                receipt["reason"] = "exact_test_registered" if valid else "missing_or_invalid_test_inventory"
                record(receipt)
                if not valid:
                    record({"id": case_id, "status": "not_run", "reason": "inventory_failed"})
                    continue
                result = command(role, case_id, ["--exact", name, "--test-threads=1", "--nocapture"])
                if result is None:
                    continue
                receipt, out, err = result
                passed = exact_test_passed(receipt, out, err, name)
                receipt["status"] = "passed" if passed else "failed"
                receipt["reason"] = "one_exact_test_passed" if passed else "libtest_contract"
                record(receipt)
        after = source_snapshot(source, git, deadline, directory)
        report["observed_source_after"] = after
        require(after == before, "candidate_hash_drift")
        for binary in request["binaries"]:
            verify_binary(binary, deadline)
        require(verify_tools(git, request["git_sha256"], request["python_sha256"], deadline) == tools,
                "tool_hash_drift")
        require(file_hash(Path(__file__).resolve(), deadline) == report["tool"]["script_sha256"]
                and file_hash(fixture_path, deadline) == report["terminal_fixture_sha256"],
                "harness_hash_drift")
        report["source_and_binary_recheck"] = "unchanged"
    except (Rejected, OSError, ValueError, KeyError, TypeError) as error:
        report["fatal"] = str(error) if isinstance(error, Rejected) else "harness_or_input_failure"
    observed = {case["id"] for case in cases}
    for case_id in expected_cases():
        if case_id not in observed:
            record({"id": case_id, "status": "not_run", "reason": "not_executed_no_coverage_claim"})
    helper_cases = {
        f"bounded-helper-tests/{name}" for name in NATIVE_TESTS["bounded-helper-tests"]
    }
    helper_results = [case for case in cases if case["id"] in helper_cases]
    helper_complete = (
        len(helper_results) == len(helper_cases)
        and all(case["status"] == "passed" for case in helper_results)
        and report.get("source_and_binary_recheck") == "unchanged"
        and "fatal" not in report
    )
    record({
        "id": "update/bounded-helper-native",
        "status": "passed" if helper_complete else "blocked",
        "reason": "exact_local_fixture_contracts_observed_not_release_acceptance"
        if helper_complete else "exact_helper_fixture_artifacts_or_native_results_missing",
    })
    for case_id, reason in [
        ("terminal/visual-editing", "requires_operator_native_terminal_evidence"),
        ("identity/broker-live-policy", "approved_live_scope_not_provided"),
        ("identity/headless-live-policy", "approved_live_scope_not_provided"),
        ("update/production-signing-publishing", "trusted_keys_and_service_refs_not_provided"),
        ("update/durable-health-rollback", "separate_runtime_recovery_acceptance_required"),
        ("clean-machine/tool-absence", "Python_verifier_host_is_not_tool_absence_proof"),
    ]:
        record({"id": case_id, "status": "blocked", "reason": reason})
    statuses = [case["status"] for case in cases]
    report["offline_result"] = (
        "failed" if "fatal" in report or "failed" in statuses
        else "partial" if "not_run" in statuses else "completed"
    )
    report["elapsed_seconds"] = round(time.monotonic() - started, 4)
    report["finished_utc"] = datetime.now(timezone.utc).isoformat()
    write_receipt(directory / "report.json", report)
    # Even a complete offline subset leaves live/signing/clean-machine gates blocked.
    return report, 1 if report["offline_result"] == "failed" else 2


def main(argv: list[str] | None = None) -> int:
    """Expose source-only capture and separately authorized native execution."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-root", required=True)
    sub = parser.add_subparsers(dest="action", required=True)
    snapshot = sub.add_parser("snapshot")
    snapshot.add_argument("--candidate-root", required=True)
    snapshot.add_argument("--git", required=True)
    snapshot.add_argument("--git-sha256", required=True)
    snapshot.add_argument("--python-sha256", required=True)
    runner = sub.add_parser("run")
    for name in ("request", "host", "lease"):
        runner.add_argument(f"--{name}", required=True)
        runner.add_argument(f"--{name}-sha256", required=True)
    runner.add_argument("--execute-trusted-candidate", action="store_true", required=True)
    args = parser.parse_args(argv)
    started = time.monotonic()
    try:
        root = absolute(args.artifact_root)
        if args.action == "snapshot":
            git = absolute(args.git)
            tools = verify_tools(git, args.git_sha256, args.python_sha256, started + 15)
            value = source_snapshot(
                absolute(args.candidate_root), git, started + 15, root,
            )
            require(verify_tools(git, args.git_sha256, args.python_sha256, started + 15) == tools,
                    "tool_hash_drift")
            print(json.dumps({"snapshot": value, "sha256": digest(canonical(value))}))
            return 0
        records = {
            name: pinned_json(absolute(getattr(args, name)), getattr(args, f"{name}_sha256"), started + 5)
            for name in ("request", "host", "lease")
        }
        with controller_slot(root):
            report, code = run(records["request"], records["host"], records["lease"], root, started)
        print(json.dumps({"classification": report["classification"], "offline_result": report["offline_result"],
                          "report": str(root / records["request"]["run_id"] / "report.json")}))
        return code
    except (Rejected, OSError, ValueError, KeyError, TypeError):
        print('{"error":"native_acceptance_rejected_no_acceptance_claim"}', file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
