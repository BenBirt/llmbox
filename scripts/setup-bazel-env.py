#!/usr/bin/env python3
"""
Populate a local Bazel Central Registry mirror and repository cache.

Fetches module metadata via api.github.com (allowed through the proxy) and
downloads source archives via github.com → objects.githubusercontent.com
(also allowed).  Bazel is then pointed at the local registry and cache so it
never needs to reach bcr.bazel.build or raw.githubusercontent.com.

Usage: python3 scripts/setup-bazel-env.py
"""

import base64
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).parent.parent.resolve()
BCR_DIR = REPO_ROOT / ".bcr"
REPO_CACHE_DIR = REPO_ROOT / ".repo-cache"

GITHUB_API = "https://api.github.com"
BCR_GITHUB_REPO = "bazelbuild/bazel-central-registry"

# Direct deps of rules_rust 0.69.0 (from its MODULE.bazel) plus rules_rust itself.
# Versions are those declared in rules_rust's MODULE.bazel.
MODULES = [
    ("rules_rust",    "0.69.0"),
    ("bazel_features","1.32.0"),
    ("bazel_skylib",  "1.8.2"),
    ("platforms",     "1.0.0"),
    ("rules_cc",      "0.2.4"),
    ("rules_license", "1.0.0"),
    ("rules_shell",   "0.6.1"),
    ("apple_support", "1.24.1"),
]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def api_get(path: str) -> dict:
    """GET from GitHub API, return parsed JSON."""
    url = f"{GITHUB_API}/{path}"
    req = urllib.request.Request(url, headers={
        "User-Agent": "llmbox-bazel-setup/1.0",
        "Accept": "application/vnd.github.v3+json",
    })
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        print(f"  HTTP {e.code} for {url}", file=sys.stderr)
        raise


def api_get_file(module: str, version: str, filename: str) -> bytes:
    """Fetch a raw file from the BCR GitHub repo via API (handles b64 encoding)."""
    path = f"repos/{BCR_GITHUB_REPO}/contents/modules/{module}/{version}/{filename}"
    try:
        data = api_get(path)
        # GitHub API returns base64-encoded content (may have newlines)
        return base64.b64decode(data["content"].replace("\n", ""))
    except Exception as e:
        print(f"  Warning: could not fetch {module}/{version}/{filename}: {e}")
        return None


def api_get_metadata(module: str) -> bytes:
    """Fetch modules/<module>/metadata.json from BCR GitHub repo."""
    path = f"repos/{BCR_GITHUB_REPO}/contents/modules/{module}/metadata.json"
    try:
        data = api_get(path)
        return base64.b64decode(data["content"].replace("\n", ""))
    except Exception:
        return None


def sri_to_hex(integrity: str) -> str:
    """Convert a SRI integrity value (sha256-<base64>) to hex sha256."""
    algo, b64 = integrity.split("-", 1)
    assert algo == "sha256"
    # Add padding if needed
    pad = len(b64) % 4
    if pad:
        b64 += "=" * (4 - pad)
    return base64.b64decode(b64, altchars=b"-_").hex()


def curl_download(url: str, dest: Path) -> bool:
    """Download a URL to dest using curl (which handles proxy auth)."""
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(".tmp")
    result = subprocess.run(
        ["curl", "-fsSL", "--max-time", "300", "-o", str(tmp), url],
        capture_output=True,
    )
    if result.returncode != 0:
        print(f"  curl failed for {url}: {result.stderr.decode()[:200]}")
        if tmp.exists():
            tmp.unlink()
        return False
    tmp.rename(dest)
    return True


def sha256_of_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def add_to_repo_cache(archive_path: Path) -> str:
    """Move/copy an archive into the Bazel repository cache by its sha256."""
    hexhash = sha256_of_file(archive_path)
    cache_file = REPO_CACHE_DIR / "content_addressable" / "sha256" / hexhash / "file"
    if not cache_file.exists():
        cache_file.parent.mkdir(parents=True, exist_ok=True)
        import shutil
        shutil.copy2(archive_path, cache_file)
        print(f"  Cached as sha256:{hexhash[:16]}…")
    else:
        print(f"  Already cached (sha256:{hexhash[:16]}…)")
    return hexhash


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def setup_module(module: str, version: str):
    print(f"\n[{module} {version}]")
    module_dir = BCR_DIR / "modules" / module / version
    module_dir.mkdir(parents=True, exist_ok=True)

    # 1. Fetch MODULE.bazel
    module_bazel_path = module_dir / "MODULE.bazel"
    if not module_bazel_path.exists():
        content = api_get_file(module, version, "MODULE.bazel")
        if content:
            module_bazel_path.write_bytes(content)
            print(f"  + MODULE.bazel")
    else:
        print(f"  . MODULE.bazel (cached)")

    # 2. Fetch source.json
    source_json_path = module_dir / "source.json"
    if not source_json_path.exists():
        content = api_get_file(module, version, "source.json")
        if content:
            source_json_path.write_bytes(content)
            print(f"  + source.json")
    else:
        print(f"  . source.json (cached)")

    # 3. Fetch metadata.json (one level up)
    metadata_path = BCR_DIR / "modules" / module / "metadata.json"
    if not metadata_path.exists():
        content = api_get_metadata(module)
        if content:
            metadata_path.write_bytes(content)
            print(f"  + metadata.json")
    else:
        print(f"  . metadata.json (cached)")

    # 4. Download source archive to repo cache
    if source_json_path.exists():
        source = json.loads(source_json_path.read_text())
        url = source.get("url", "")
        integrity = source.get("integrity", "")
        if url and integrity:
            hexhash = sri_to_hex(integrity)
            cache_file = REPO_CACHE_DIR / "content_addressable" / "sha256" / hexhash / "file"
            if cache_file.exists():
                print(f"  . archive (cached)")
            else:
                print(f"  Downloading archive: {url}")
                tmp = Path(f"/tmp/bcr-archive-{module}-{version}.tar.gz")
                if curl_download(url, tmp):
                    actual_hash = add_to_repo_cache(tmp)
                    if actual_hash != hexhash:
                        print(f"  WARNING: hash mismatch! expected {hexhash}, got {actual_hash}")
                    tmp.unlink(missing_ok=True)
                else:
                    print(f"  WARNING: archive download failed for {module} {version}")


def setup_registry_root():
    """Create the root bazel_registry.json."""
    root_json = BCR_DIR / "bazel_registry.json"
    if not root_json.exists():
        root_json.write_text(json.dumps({"mirrors": []}, indent=2) + "\n")
        print("+ bazel_registry.json")


def main():
    print(f"BCR mirror:    {BCR_DIR}")
    print(f"Repo cache:    {REPO_CACHE_DIR}")

    BCR_DIR.mkdir(parents=True, exist_ok=True)
    REPO_CACHE_DIR.mkdir(parents=True, exist_ok=True)

    setup_registry_root()

    for module, version in MODULES:
        setup_module(module, version)

    print("\nDone.")
    print(f"\nAdd to .bazelrc:")
    print(f"  common --registry=file://{BCR_DIR}")
    print(f"  common --repository_cache={REPO_CACHE_DIR}")


if __name__ == "__main__":
    main()
