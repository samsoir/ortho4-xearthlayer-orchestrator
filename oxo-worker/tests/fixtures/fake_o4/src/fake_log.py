"""Shared recorder/controller for the fake O4 tree, driven by env vars:
FAKE_O4_LOG (file to append records to), FAKE_O4_FAIL (function name that
raises), FAKE_O4_NOISE (print to stdout inside build functions)."""
import os, sys


def rec(line):
    p = os.environ.get("FAKE_O4_LOG")
    if p:
        with open(p, "a") as f:
            f.write(line + "\n")


def enter(name):
    rec("call " + name)
    if os.environ.get("FAKE_O4_NOISE"):
        print("noise from " + name)
    if os.environ.get("FAKE_O4_FAIL") == name:
        raise RuntimeError("boom in " + name)
