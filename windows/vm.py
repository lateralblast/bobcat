#!/usr/bin/env python3

# Name:         vm (builds the bobcat Windows installer in a Windows 11 virtual machine run by Docker)
# Version:      0.0.1
# Release:      1
# License:      CC BY-NC-SA (Creative Commons Attribution-NonCommercial-ShareAlike 4.0)
#               http://creativecommons.org/licenses/by-nc-sa/4.0/legalcode
# Group:        System
# Source:       N/A
# URL:          http://lateralblast.com.au/
# Distribution: UNIX
# Vendor:       UNIX
# Packager:     Richard Spindler <richard@lateralblast.com.au>
# Description:  Starts the dockurr/windows image (Windows 11 under QEMU/KVM), has it build the GUI installer and bobcat-cli, and collects them in windows/out/bundle

import argparse
import os
import platform
import re
import shutil
import subprocess
import sys
import time

HEADER = os.path.abspath(__file__)
HERE = os.path.dirname(HEADER)
COMPOSE = os.path.join(HERE, "compose.yml")
OUT = os.path.join(HERE, "out")
REQUEST = os.path.join(OUT, "build.request")
DONE = os.path.join(OUT, "build.done")
LOG = os.path.join(OUT, "build.log")
READY = os.path.join(OUT, "ready")


def get_header(name):
    with open(HEADER, encoding="utf-8") as handle:
        match = re.search(r"^#\s+%s:\s+(.*)$" % name, handle.read(), re.M)
    return match.group(1).strip() if match else ""


def compose(*args, check=True):
    return subprocess.run(["docker", "compose", "-f", COMPOSE] + list(args), check=check)


def running():
    out = subprocess.run(["docker", "inspect", "-f", "{{.State.Running}}", "bobcat-windows"], capture_output=True, text=True)
    return out.stdout.strip() == "true"


def require_linux():
    """The VM needs KVM, which only Linux has, so refuse to start anything anywhere else."""
    if platform.system() == "Linux":
        return
    here = {"Darwin": "macOS"}.get(platform.system(), platform.system())
    native = "run: ./build.sh --target windows (Windows, in Git Bash)" if here == "Windows" else "build on a Linux machine"
    sys.exit("This builds the Windows packages in a virtual machine that needs Linux and KVM, so it can't run on %s.\n"
             "%s" % (here, "For a native build of this OS on this machine, " + native + "."))


def preflight():
    problems = []
    if not shutil.which("docker"):
        problems.append("docker is not installed")
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        problems.append("/dev/kvm is missing or not accessible (the VM needs KVM; is your user in the kvm group?)")
    if problems:
        sys.exit("Can't run the Windows VM:\n  " + "\n  ".join(problems))


def stage_scripts():
    """Copy the oem scripts to out/oem, where a running VM can reach them (it doesn't reliably see new files in the src share)."""
    os.makedirs(os.path.join(OUT, "oem"), exist_ok=True)
    for name in os.listdir(os.path.join(HERE, "oem")):
        shutil.copy(os.path.join(HERE, "oem", name), os.path.join(OUT, "oem", name))


def up(quiet=False):
    preflight()
    os.makedirs(OUT, exist_ok=True)
    stage_scripts()
    compose("up", "-d")
    if quiet:
        return
    print("\nThe first start downloads Windows (about 5 GB), installs it, then installs the Visual Studio Build\n"
          "Tools, Rust, Perl and tauri-cli, which takes a good while. Watch it at http://127.0.0.1:8007 and check with: vm.py status\n"
          "Windows is Microsoft's software, used under its own licence terms.")


def status():
    state = "running" if running() else "not running"
    ready = os.path.exists(READY)
    print("VM: %s; build tools: %s" % (state, "installed, ready to build" if ready else "not ready yet (see http://127.0.0.1:8007, or out/install.log)"))
    return state == "running" and ready


def read_from(path, offset):
    if not os.path.exists(path):
        return offset
    with open(path, "rb") as handle:
        handle.seek(offset)
        chunk = handle.read()
    sys.stdout.write(chunk.decode("utf-8", errors="replace").replace("\r", ""))
    sys.stdout.flush()
    return offset + len(chunk)


def build_managed(build_args, timeout):
    """Start the VM if it is not running (its build tools must already be installed), build, and stop it again
    if this started it, also after a failed or interrupted build. A VM that was already running is left running."""
    started = False
    if not running():
        if not os.path.exists(READY):
            sys.exit("The VM has never finished installing its build tools. Run: python3 %s up   and wait (see status)" % os.path.relpath(HEADER))
        print("Starting the Windows VM...")
        up(quiet=True)
        started = True
    try:
        build(build_args, timeout)
    finally:
        if started:
            print("Stopping the Windows VM (its disk is kept)...")
            compose("down", check=False)


def build(build_args, timeout):
    if not status():
        sys.exit("Run: python3 %s up   and wait until the build tools are installed" % os.path.relpath(HEADER))
    stage_scripts()  # the VM runs build.cmd from here (C:\\bobcat\\build.cmd is only a stub)
    for path in (DONE, LOG):
        if os.path.exists(path):
            os.remove(path)
    with open(REQUEST, "w", encoding="ascii", newline="\r\n") as handle:  # the watcher picks this up within a few seconds
        handle.write(" ".join(build_args) + "\n")
    offset = 0
    deadline = time.time() + timeout
    while not os.path.exists(DONE):
        offset = read_from(LOG, offset)
        if time.time() > deadline:
            sys.exit("\nTimed out after %d minutes; the build may still be running (out/build.log)" % (timeout // 60))
        time.sleep(2)
    read_from(LOG, offset)
    with open(DONE, encoding="ascii", errors="replace") as handle:
        code = handle.read().strip()
    if code != "0":
        sys.exit("\nThe Windows build failed (exit code %s); see %s" % (code, LOG))
    print("\nInstaller and CLI are in %s" % os.path.join(OUT, "bundle"))


def main():
    parser = argparse.ArgumentParser(description="Build the bobcat Windows installer in a Windows 11 VM (dockurr/windows)")
    parser.add_argument("-V", "--version", action="store_true", help="Show the version of this script and exit")
    sub = parser.add_subparsers(dest="command")
    sub.add_parser("up", help="Start the VM (the first start installs Windows and the build tools)")
    sub.add_parser("status", help="Show whether the VM is running and the build tools are installed")
    sub.add_parser("down", help="Stop the VM; its disk is kept, so the next up is quick")
    build_parser = sub.add_parser("build", help="Build the GUI installer and bobcat-cli in the VM")
    build_parser.add_argument("--timeout", type=int, default=7200, help="Give up waiting after this many seconds (default 7200)")
    build_parser.add_argument("--manage", action="store_true", help="Start the VM first if it is not running and stop it after the build (only if this started it)")
    build_parser.add_argument("build_args", nargs="*", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.version:
        print("%s v. %s %s" % (get_header("Name"), get_header("Version"), get_header("Packager")))
        return
    if args.command:
        require_linux()
    if args.command == "up":
        up()
    elif args.command == "status":
        sys.exit(0 if status() else 1)
    elif args.command == "down":
        compose("down")
    elif args.command == "build":
        (build_managed if args.manage else build)(args.build_args, args.timeout)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
