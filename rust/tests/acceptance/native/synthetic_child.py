# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Synthetic subprocess adversaries; no product, provider or credential access."""

import os
import sys
import time

if sys.argv[1] == "wait":
    time.sleep(30)
elif sys.argv[1] == "flood":
    os.write(1, b"x" * (64 * 1024 + 1))
elif sys.argv[1] == "secret":
    print("synthetic-secret-must-not-be-in-receipt")
elif sys.argv[1] == "empty-tests":
    print("test result: ok. 0 passed; 0 failed; 0 ignored; 1 filtered out")
elif sys.argv[1] == "cursor-flood":
    import tty

    tty.setraw(0)
    os.write(1, b"$ ")
    while True:
        os.write(1, b"\x1b[6n" * 1024)
elif sys.argv[1] in ("cursor-only-exit", "clean-eof"):
    import select
    import termios
    import tty

    before = termios.tcgetattr(0)
    expected = b"\x1b[1;1R" if sys.argv[1] == "cursor-only-exit" else b"\x04"
    received = bytearray()
    try:
        tty.setraw(0)
        os.write(1, b"\x1b[6n" if sys.argv[1] == "cursor-only-exit" else b"$ ")
        deadline = time.monotonic() + 1
        while len(received) < len(expected) and time.monotonic() < deadline:
            if select.select([0], [], [], max(0, deadline - time.monotonic()))[0]:
                received.extend(os.read(0, len(expected) - len(received)))
    finally:
        termios.tcsetattr(0, termios.TCSANOW, before)
    raise SystemExit(0 if received == expected else 3)
else:
    raise SystemExit(2)
