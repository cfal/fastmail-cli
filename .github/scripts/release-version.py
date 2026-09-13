import json
import os
import re
import subprocess
import tomllib


def version_tuple(value):
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", value)
    if not match:
        raise ValueError(f"Expected a stable semantic version, got {value!r}")
    return tuple(map(int, match.groups()))


def is_new_release(version, releases):
    current = version_tuple(version)
    published = []
    for release in releases:
        if release["draft"] or release["prerelease"]:
            continue
        try:
            published.append(version_tuple(release["tag_name"]))
        except ValueError:
            continue
    if published and current < max(published):
        raise ValueError("Refusing to publish a version older than an existing release")
    return current not in published


if __name__ == "__main__":
    with open("Cargo.toml", "rb") as manifest:
        version = tomllib.load(manifest)["package"]["version"]
    pages = json.loads(subprocess.check_output([
        "gh", "api", f"repos/{os.environ['GITHUB_REPOSITORY']}/releases",
        "--paginate", "--slurp",
    ], text=True))
    is_new = is_new_release(version, [release for page in pages for release in page])
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"version={version}\ntag=v{version}\nis_new={str(is_new).lower()}\n")
    print(f"Version {version}, new release: {is_new}")
