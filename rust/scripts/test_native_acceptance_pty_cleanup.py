# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Unit checks of PTY draining; not native process-cleanup acceptance."""

import errno
import select
import subprocess
import sys
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, call, patch

import native_acceptance as acceptance


class PtyWaitTests(unittest.TestCase):
    def test_wait_drains_pending_output_until_the_owned_child_is_reaped(self) -> None:
        child = SimpleNamespace(args=["synthetic"], poll=Mock(side_effect=[None, None, -9]))
        with patch.object(select, "select", return_value=([123], [], [])), \
             patch.object(acceptance.os, "read", return_value=b"queued terminal output") as read:
            self.assertEqual(acceptance.wait_owned_process(child, 1, 123), -9)
        self.assertEqual(read.call_args_list, [call(123, 4096)] * 2)

    def test_wait_retains_the_existing_deadline_when_the_child_is_not_reaped(self) -> None:
        child = SimpleNamespace(args=["synthetic"], poll=Mock(return_value=None))
        with patch.object(acceptance.time, "monotonic", side_effect=[100, 100, 100.005, 100.011]), \
             patch.object(select, "select", return_value=([], [], [])):
            with self.assertRaises(subprocess.TimeoutExpired):
                acceptance.wait_owned_process(child, 0.01, 123)
        self.assertEqual(child.poll.call_count, 3)

    def test_descriptor_failure_is_not_reported_as_reaped(self) -> None:
        child = SimpleNamespace(args=["synthetic"], poll=Mock(return_value=None))
        with patch.object(select, "select", return_value=([123], [], [])), \
             patch.object(acceptance.os, "read", side_effect=OSError(errno.EBADF, "synthetic")):
            with self.assertRaises(OSError):
                acceptance.wait_owned_process(child, 1, 123)

    def test_selector_range_failure_uses_the_existing_cleanup_error_path(self) -> None:
        child = SimpleNamespace(args=["synthetic"], poll=Mock(return_value=None))
        with patch.object(select, "select", side_effect=ValueError("synthetic descriptor range")):
            with self.assertRaises(OSError) as rejected:
                acceptance.wait_owned_process(child, 1, 123)
        self.assertEqual(rejected.exception.errno, errno.EINVAL)
        self.assertEqual(rejected.exception.strerror, "Owned PTY selector failed")

    def test_unavailable_terminal_state_remains_explicit_and_cannot_prove_restoration(self) -> None:
        read = Mock()
        module = SimpleNamespace(tcgetattr=read, error=OSError)
        with patch.dict(sys.modules, {"termios": module}):
            for value, expected in [([1, 2], (True, None)), ([1, 3], (False, None))]:
                read.return_value = value
                self.assertEqual(acceptance.terminal_state(123, [1, 2]), expected)
            read.side_effect = OSError(errno.EIO, "synthetic hung-up terminal")
            self.assertEqual(
                acceptance.terminal_state(123, [1, 2]),
                (False, "terminal_state_unavailable"),
            )
