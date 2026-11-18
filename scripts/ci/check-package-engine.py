#!/usr/bin/env python3
"""Fails (exit 1) when a packaged app.jar does not contain the native engine.

Usage: check-package-engine.py <path to app.jar> <entry> [<entry> ...]

The Windows package was once built without its Rust library and programs because a build script ignored a failed step,
and the app then crashed at start-up. This reads the finished jar so such a package can never pass again.
"""
import sys
import zipfile

if len(sys.argv) < 3:
    print("usage: check-package-engine.py <app.jar> <entry> [<entry> ...]")
    sys.exit(2)

jar, needed = sys.argv[1], sys.argv[2:]
try:
    with zipfile.ZipFile(jar) as z:
        names = set(z.namelist())
except Exception as error:  # a missing or unreadable file is a failure too
    print(f"::error title=package engine::cannot read {jar}: {error}")
    sys.exit(1)

missing = [name for name in needed if name not in names]
if missing:
    print(f"::error title=package engine::{jar} is missing: {', '.join(missing)}")
    sys.exit(1)
print(f"::notice title=package engine::{jar} contains {', '.join(needed)}")
