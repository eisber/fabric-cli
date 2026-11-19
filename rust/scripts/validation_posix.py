# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded termination of owned POSIX descendants, including new sessions."""

from __future__ import annotations

import ctypes
import errno
import os
import secrets
import signal
import struct
import time
from pathlib import Path
from typing import Any


class LinuxTree:
    """Adopt, kill by pidfd, and reap every descendant until the kernel says ECHILD."""

    method = "linux-subreaper-pidfd"

    def __init__(self) -> None:
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(36, 1, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "Cannot establish validation subreaper")
        if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
            raise RuntimeError("Owned Linux cleanup requires pidfd support")

    def observe(self, pid: int) -> None:
        """Linux preserves ownership across reparenting without polling ancestry."""

    def terminate(self, pid: int, deadline: float) -> list[str]:
        """The root group is signalled first; adoption/reaping handles escaped sessions."""
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except OSError:
            return ["owned_process_group_signal_failed"]
        return []

    def confirm(self, deadline: float) -> list[str]:
        """Kill only identity-bound adopted children and drain all owned wait statuses."""
        errors: list[str] = []
        while time.monotonic() < deadline:
            try:
                # ECHILD is the completion fence, not an empty /proc snapshot.
                while os.waitpid(-1, os.WNOHANG)[0]:
                    pass
            except ChildProcessError:
                return errors
            try:
                children: set[int] = set()
                for task in Path("/proc/self/task").iterdir():
                    try:
                        children.update(
                            map(int, (task / "children").read_text().split())
                        )
                    except FileNotFoundError:
                        continue
                for pid in children:
                    if time.monotonic() >= deadline:
                        break
                    descriptor = None
                    try:
                        descriptor = os.pidfd_open(pid)
                        # The pidfd prevents PID reuse between the ownership
                        # check and signalling. Other processes are never targets.
                        fields = (
                            Path(f"/proc/{pid}/stat")
                            .read_text()
                            .rsplit(")", 1)[1]
                            .split()
                        )
                        if int(fields[1]) != os.getpid():
                            errors.append("adopted_child_identity_unconfirmed")
                            continue
                        signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                    except (ProcessLookupError, FileNotFoundError):
                        pass
                    except OSError as error:
                        if error.errno != errno.ESRCH:
                            errors.append("adopted_child_signal_failed")
                    finally:
                        if descriptor is not None:
                            os.close(descriptor)
            except OSError:
                errors.append("owned_children_inventory_failed")
            time.sleep(0.01)
        return list(dict.fromkeys([*errors, "owned_descendant_reap_unconfirmed"]))


def _darwin_has_marker(data: bytes, marker: bytes) -> bool:
    """Match one exact environment entry in KERN_PROCARGS2, never an argv substring."""
    if len(data) < 4:
        raise ValueError("Truncated Darwin process arguments")
    argc = struct.unpack_from("=i", data)[0]
    if argc < 1:
        raise ValueError("Invalid Darwin argument count")
    offset = data.find(b"\0", 4)
    if offset < 0:
        raise ValueError("Missing Darwin executable terminator")
    while offset < len(data) and data[offset] == 0:
        offset += 1
    for _ in range(argc):
        offset = data.find(b"\0", offset)
        if offset < 0:
            raise ValueError("Truncated Darwin argument vector")
        offset += 1
    for entry in data[offset:].split(b"\0"):
        if not entry:
            break
        if entry == marker:
            return True
    return False


class DarwinTree:
    """Clean an inherited-marker cohort, including reparented/new-session children.

    This is cooperative build-process cleanup, not a sandbox: descendants must
    preserve their command's private environment marker. Observed marker loss or
    unreadable candidate identities/environments fails closed. An unobserved
    orphan that deliberately removes the marker cannot be attributed on Darwin.
    No runner-wide marker, process name, or unrelated process group is signalled.
    Darwin has no pidfd; start identities are rechecked immediately before kill.
    """

    method = "darwin-inherited-marker-starttime"

    def __init__(self) -> None:
        class Info(ctypes.Structure):
            _fields_ = (
                [
                    (name, ctypes.c_uint32)
                    for name in (
                        "flags",
                        "status",
                        "exit",
                        "pid",
                        "ppid",
                        "uid",
                        "gid",
                        "ruid",
                        "rgid",
                        "svuid",
                        "svgid",
                        "reserved",
                    )
                ]
                + [
                    ("command", ctypes.c_char * 16),
                    ("name", ctypes.c_char * 32),
                ]
                + [
                    (name, ctypes.c_uint32)
                    for name in ("files", "group", "jobs", "device", "foreground")
                ]
                + [
                    ("nice", ctypes.c_int32),
                    ("seconds", ctypes.c_uint64),
                    ("microseconds", ctypes.c_uint64),
                ]
            )

        self.info_type = Info
        self.api = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self.api.proc_pidinfo.argtypes = [
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_uint64,
            ctypes.c_void_p,
            ctypes.c_int,
        ]
        self.api.proc_pidinfo.restype = ctypes.c_int
        self.api.proc_listpids.argtypes = [
            ctypes.c_int,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.c_int,
        ]
        self.api.proc_listpids.restype = ctypes.c_int
        self.libc = ctypes.CDLL(None, use_errno=True)
        self.libc.sysctl.argtypes = [
            ctypes.POINTER(ctypes.c_int),
            ctypes.c_uint,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_size_t),
            ctypes.c_void_p,
            ctypes.c_size_t,
        ]
        self.libc.sysctl.restype = ctypes.c_int
        argmax = ctypes.c_int()
        size = ctypes.c_size_t(ctypes.sizeof(argmax))
        if (
            self.libc.sysctl(
                (ctypes.c_int * 2)(1, 8),
                2,
                ctypes.byref(argmax),
                ctypes.byref(size),
                None,
                0,
            )
            != 0
            or not 0 < argmax.value <= 16 * 1024 * 1024
        ):
            raise OSError("Darwin argument capacity unavailable")
        self.argmax = argmax.value
        # A fresh key also preserves enclosing supervisors' inherited markers.
        self.environment = {f"FABRIC_VALIDATION_PROCESS_{secrets.token_hex(32)}": "1"}
        self.marker = next(
            f"{key}={value}".encode() for key, value in self.environment.items()
        )
        self.owned: dict[int, tuple[int, int]] = {}
        self.errors: list[str] = []
        self.root: int | None = None
        parent = self._info(os.getpid())
        if parent is None:
            raise OSError("Darwin supervisor identity unavailable")
        self.started = (parent.seconds, parent.microseconds)

    def _info(self, pid: int) -> Any:
        info = self.info_type()
        ctypes.set_errno(0)
        count = self.api.proc_pidinfo(
            pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info)
        )
        if count == ctypes.sizeof(info):
            return info
        if count == 0 and ctypes.get_errno() in (errno.ESRCH, errno.ENOENT):
            return None
        raise OSError("Owned Darwin process identity unavailable")

    def _pids(self, kind: int, value: int, deadline: float) -> list[int]:
        ctypes.set_errno(0)
        size = self.api.proc_listpids(kind, value, None, 0)
        if size < 0 or (size == 0 and ctypes.get_errno()):
            raise OSError("Darwin process inventory unavailable")
        for _ in range(4):
            if time.monotonic() >= deadline:
                raise OSError("Darwin process inventory deadline")
            buffer = (ctypes.c_int * (size // 4 + 128))()
            ctypes.set_errno(0)
            count = self.api.proc_listpids(kind, value, buffer, ctypes.sizeof(buffer))
            if count < 0 or (count == 0 and ctypes.get_errno()):
                raise OSError("Darwin process inventory unavailable")
            if count < ctypes.sizeof(buffer):
                return [int(value) for value in buffer[: count // 4] if value > 0]
            size = count * 2
        raise OSError("Darwin process inventory was truncated")

    def _has_marker(self, pid: int) -> bool:
        buffer = ctypes.create_string_buffer(self.argmax)
        size = ctypes.c_size_t(self.argmax)
        # CTL_KERN, KERN_PROCARGS2: argc, executable, padding, argv, environ.
        if (
            self.libc.sysctl(
                (ctypes.c_int * 3)(1, 49, pid),
                3,
                buffer,
                ctypes.byref(size),
                None,
                0,
            )
            != 0
        ):
            raise OSError("Darwin process environment unavailable")
        return _darwin_has_marker(buffer.raw[: size.value], self.marker)

    def _same(self, pid: int) -> Any:
        info = self._info(pid)
        if info and (info.seconds, info.microseconds) == self.owned.get(pid):
            return info
        return None

    def observe(self, pid: int) -> None:
        """Remember descendants before their parents can exit or change sessions."""
        self.root = pid
        queue = list(self.owned) or [pid]
        seen: set[int] = set()
        deadline = time.monotonic() + 0.02
        try:
            while queue and time.monotonic() < deadline:
                current = queue.pop()
                if current in seen:
                    continue
                seen.add(current)
                info = self._info(current)
                if info is None:
                    continue
                identity = (info.seconds, info.microseconds)
                if current in self.owned and self.owned[current] != identity:
                    continue
                self.owned[current] = identity
                if info.status == 5:
                    continue
                for child in self._pids(6, current, deadline):
                    child_info = self._info(child)
                    if (
                        child_info
                        and child_info.ppid == current
                        and self._same(current)
                    ):
                        self.owned[child] = (
                            child_info.seconds,
                            child_info.microseconds,
                        )
                        queue.append(child)
        except OSError:
            # Observation is only an optimization. The final marker inventory
            # owns the cleanup fence, including parents that exited unseen.
            pass

    def _discover(self, deadline: float) -> None:
        # A unique inherited marker survives setsid(), double-fork and parent
        # exit. Only newly born same-uid candidates' private environments are
        # inspected, in memory; neither their contents nor the marker is logged.
        for pid in self._pids(4, os.getuid(), deadline):
            if time.monotonic() >= deadline:
                raise OSError("Darwin marker inventory deadline")
            if pid == os.getpid():
                continue
            info = self._info(pid)
            if info is None or info.status == 5:
                continue
            identity = (info.seconds, info.microseconds)
            if identity < self.started:
                continue
            try:
                marked = self._has_marker(pid)
            except (OSError, ValueError):
                latest = self._info(pid)
                if (
                    latest
                    and latest.status != 5
                    and (latest.seconds, latest.microseconds) == identity
                ):
                    self.errors.append("darwin_candidate_environment_unavailable")
                continue
            latest = self._info(pid)
            if latest is None or (latest.seconds, latest.microseconds) != identity:
                continue
            if marked:
                self.owned[pid] = identity
            elif self.owned.get(pid) == identity:
                self.errors.append("darwin_owned_tracking_marker_missing")

    def terminate(self, pid: int, deadline: float) -> list[str]:
        """Freeze the marked cohort, rescan escaped forks, then kill owned identities."""
        frozen: set[int] = set()

        def freeze(current: int) -> None:
            if current in frozen:
                return
            if time.monotonic() >= deadline:
                self.errors.append("owned_descendant_freeze_deadline")
                return
            info = self._same(current)
            if info is None or info.status == 5:
                return
            os.kill(current, signal.SIGSTOP)
            while time.monotonic() < deadline:
                info = self._same(current)
                if info is None or info.status == 5:
                    return
                if info.status == 4:
                    break
                time.sleep(0.005)
            else:
                self.errors.append("owned_descendant_freeze_deadline")
                return
            frozen.add(current)
            for child in self._pids(6, current, deadline):
                child_info = self._info(child)
                if child_info and child_info.ppid == current:
                    self.owned[child] = (child_info.seconds, child_info.microseconds)
                    freeze(child)

        try:
            self._discover(deadline)
            while time.monotonic() < deadline:
                for current in list(self.owned):
                    try:
                        freeze(current)
                    except ProcessLookupError:
                        pass
                # This second full inventory catches forks reparented between
                # the first snapshot and freezing their original ancestors.
                self._discover(deadline)
                if all(
                    current in frozen
                    or not (info := self._same(current))
                    or info.status == 5
                    for current in self.owned
                ):
                    break
            else:
                self.errors.append("owned_descendant_freeze_deadline")
        except OSError:
            self.errors.append("owned_descendant_inventory_unconfirmed")
        for current in reversed(list(self.owned)):
            try:
                info = self._same(current)
                if info and info.status != 5:
                    os.kill(current, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError:
                self.errors.append("owned_descendant_signal_unconfirmed")
        return list(dict.fromkeys(self.errors))

    def confirm(self, deadline: float) -> list[str]:
        """Observe disappearance of every recorded identity, including separate sessions."""
        while time.monotonic() < deadline:
            try:
                self._discover(deadline)
                if not any(self._same(pid) for pid in self.owned):
                    return list(dict.fromkeys(self.errors))
                for pid in self.owned:
                    info = self._same(pid)
                    if info and info.status != 5:
                        os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                continue
            except OSError:
                self.errors.append("owned_descendant_identity_unconfirmed")
                break
            time.sleep(0.01)
        return list(dict.fromkeys([*self.errors, "owned_descendant_exit_unconfirmed"]))
