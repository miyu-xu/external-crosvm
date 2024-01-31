#!/usr/bin/env python3
# -*- coding: utf-8 -*-
# Copyright 2024 - The Android Open Source Project
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
import subprocess
from pathlib import Path
import shutil
import platform
import argparse
import os

here = Path(__file__).resolve().parent


def reverse_version_iterator(version_str):
    """
    Creates a reversed iterator that yields the versions in decreasing order.

    Args:
      version_str: A string representing a version number, e.g., "0.1.2.3".

    Yields:
      Strings representing the versions in reverse order,
      e.g., "0.1.2.3", "0.1.2", "0.1", "0".
    """
    parts = version_str.split(".")
    for i in range(len(parts), 0, -1):
        yield ".".join(parts[:i])


def shared_ext(args):
    target = args.target
    if target == "windows":
        return ["dll"]
    if target == "linux":
        return [f"so.{v}" for v in reverse_version_iterator(args.version)] + ['so']
    if target == "darwin":
        return [f"{v}.dylib" for v in reverse_version_iterator(args.version)] + ['.dylib']
    raise ValueError(f"Unsupported target: {target}")


def install(args):
    lib_dest_dir = Path(args.dest).absolute()
    lib_dest_dir.mkdir(parents=True, exist_ok=True)

    exts = shared_ext(args)
    shl = f"librutabaga_gfx_ffi.{exts[-1]}"
    src = here / "target" / "release" / shl
    shutil.copy(src, lib_dest_dir)
    shutil.copy(src, lib_dest_dir)

    for ext in exts[:-1]:
        lns = (lib_dest_dir / f"librutabaga_gfx_ffi.{ext}")
        if not lns.exists():
            lns.symlink_to(shl)



def build(args):
    env = os.environ

    if args.lib[0] == "-":
        libdir = Path(args.lib[2:]).parent
    else:
        libdir = Path(args.lib).parent

    env["GFXSTREAM_PATH"] = str(libdir)
    subprocess.check_call(["cargo", "build", "-r", "--features=gfxstream"], cwd=here, env=env)
    install(args)


def binplace(args):
    inc_dest_dir = Path(args.dest) / "rutabaga_gfx"
    inc_dest_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy(here / "src" / "include" / "rutabaga_gfx_ffi.h", inc_dest_dir)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Build and install Rutabaga GFX FFI library.")
    parser.add_argument("--dest", default="", help="Installation prefix (DESTDIR).")
    subparsers = parser.add_subparsers(required=True)

    header = subparsers.add_parser(
        "header", help="Binplaces the rutabaga_gfx_ffi.h header to the destination."
    )
    header.set_defaults(func=binplace)

    builder = subparsers.add_parser(
        "build",
        help="Invokes cargo build with --features=gfxstream.",
    )
    builder.add_argument("--lib", help="Path to gfxstream backend", required=True)
    builder.add_argument("--version", help="Version of the created shared library", required=True)
    builder.add_argument("--target", help="Target os for the created library", required=True)
    builder.set_defaults(func=build)

    args = parser.parse_args()
    args.func(args)
