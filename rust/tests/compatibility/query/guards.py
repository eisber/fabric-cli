# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Audit guards shared by reference comparisons and negative-control tests."""

import contextlib
import os
import sys
from typing import Any, Iterator

ACTIVE = False


def audit(event: str, args: tuple[Any, ...]) -> None:
    """Reject network, auth registry, processes, sleeps and disk mutation."""
    if not ACTIVE:
        return
    if (
        event.startswith(("socket.", "subprocess.", "winreg."))
        or event in {
            "os.system", "os.exec", "os.posix_spawn", "os.spawn", "os.fork",
            "os.putenv", "os.unsetenv", "os.mkdir", "os.remove", "os.rmdir",
            "os.rename", "os.link", "os.symlink", "os.truncate", "time.sleep",
        }
    ):
        raise AssertionError(f"denied side effect: {event}")
    if event == "open":
        _, mode, flags = args
        if (mode and any(c in mode for c in "wax+")) or flags & (
            os.O_WRONLY | os.O_RDWR | os.O_APPEND | os.O_CREAT | os.O_TRUNC
        ):
            raise AssertionError("denied disk write")


sys.addaudithook(audit)


@contextlib.contextmanager
def offline() -> Iterator[None]:
    """Activate guards for a bounded evaluation without changing product modules."""
    global ACTIVE
    previous = ACTIVE
    ACTIVE = True
    try:
        yield
    finally:
        ACTIVE = previous
