#!/usr/bin/env python3
"""Prepare an Input Dynamics Keyboard GitHub release."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BUILD_GRADLE = ROOT / "app" / "build.gradle.kts"
SEMVER_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
UPSTREAM_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def run(args: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=ROOT,
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def require_clean_worktree() -> None:
    status = run(["git", "status", "--short"]).stdout.strip()
    if status:
        print("Working tree is not clean. Commit or stash changes before releasing.", file=sys.stderr)
        print(status, file=sys.stderr)
        sys.exit(1)


def require_tag_absent(tag: str) -> None:
    result = run(["git", "rev-parse", "-q", "--verify", f"refs/tags/{tag}"], check=False)
    if result.returncode == 0:
        print(f"Tag already exists: {tag}", file=sys.stderr)
        sys.exit(1)


def version_code(version: str) -> int:
    major, minor, patch = (int(part) for part in version.split("."))
    if major > 20 or minor > 999 or patch > 999:
        print("Version is too large for the release versionCode scheme.", file=sys.stderr)
        sys.exit(1)
    return 100_000 + major * 1_000_000 + minor * 1_000 + patch


def update_gradle_version(version: str, upstream: str) -> None:
    text = BUILD_GRADLE.read_text()
    android_version_name = f"{version}+heli{upstream}"
    android_version_code = version_code(version)

    text, code_count = re.subn(
        r"versionCode = \d+",
        f"versionCode = {android_version_code}",
        text,
        count=1,
    )
    text, name_count = re.subn(
        r'versionName = "[^"]+"',
        f'versionName = "{android_version_name}"',
        text,
        count=1,
    )

    if code_count != 1 or name_count != 1:
        print(f"Could not update version fields in {BUILD_GRADLE}", file=sys.stderr)
        sys.exit(1)

    BUILD_GRADLE.write_text(text)


def main() -> None:
    parser = argparse.ArgumentParser(description="Prepare and optionally publish a GitHub release tag.")
    parser.add_argument("version", help="Fork SemVer version, for example 0.1.0")
    parser.add_argument(
        "--upstream",
        default="3.9",
        help="HeliBoard base version used in Android versionName metadata. Default: 3.9",
    )
    parser.add_argument(
        "--push",
        action="store_true",
        help="Push main and the release tag to origin after committing.",
    )
    parser.add_argument(
        "--no-verify",
        action="store_true",
        help="Skip the local release verification command.",
    )
    args = parser.parse_args()

    if not SEMVER_RE.fullmatch(args.version):
        print("Version must be plain SemVer: MAJOR.MINOR.PATCH", file=sys.stderr)
        sys.exit(1)
    if not UPSTREAM_RE.fullmatch(args.upstream):
        print("Upstream version may contain only letters, digits, dot, underscore, or hyphen.", file=sys.stderr)
        sys.exit(1)

    tag = f"v{args.version}"
    require_clean_worktree()
    require_tag_absent(tag)

    update_gradle_version(args.version, args.upstream)

    if not args.no_verify:
        print("Running release verification...")
        verify = subprocess.run(
            ["./gradlew", ":app:testRunTestsUnitTest", ":app:assembleDebug"],
            cwd=ROOT,
        )
        if verify.returncode != 0:
            sys.exit(verify.returncode)

    run(["git", "add", str(BUILD_GRADLE.relative_to(ROOT))])
    run(["git", "commit", "-m", f"Release {tag}"])
    run(["git", "tag", "-a", tag, "-m", f"Input Dynamics Keyboard {tag}"])

    print(f"Prepared {tag}")
    print(f"Android versionName: {args.version}+heli{args.upstream}")
    print(f"Android versionCode: {version_code(args.version)}")

    if args.push:
        run(["git", "push", "origin", "main"])
        run(["git", "push", "origin", tag])
        print(f"Pushed main and {tag}. GitHub Actions will publish the debug APK release.")
    else:
        print("Review the commit, then push with:")
        print("  git push origin main")
        print(f"  git push origin {tag}")


if __name__ == "__main__":
    main()
