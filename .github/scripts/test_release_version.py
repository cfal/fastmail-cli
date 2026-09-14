import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("release_version", Path(__file__).with_name("release-version.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
SCRIPT = Path(__file__).with_name("release-version.py").resolve()


class ReleaseVersionTests(unittest.TestCase):
    def test_repository_manifest(self):
        with (Path(__file__).resolve().parents[2] / "Cargo.toml").open("rb") as manifest:
            version = module.tomllib.load(manifest)["package"]["version"]
        self.assertTrue(module.is_new_release(version, []))

    def test_ordering_and_existing_releases(self):
        releases = [{"tag_name": "v3.10.0", "draft": False, "prerelease": False}]
        self.assertTrue(module.is_new_release("4.0.0", releases))
        self.assertFalse(module.is_new_release("3.10.0", releases))
        with self.assertRaises(ValueError):
            module.is_new_release("3.9.0", releases)

    def test_drafts_can_be_completed(self):
        self.assertTrue(module.is_new_release("4.0.0", [
            {"tag_name": "v4.0.0", "draft": True, "prerelease": False},
            {"tag_name": "v5.0.0-rc.1", "draft": False, "prerelease": True},
        ]))

    def test_release_tags_are_compared_numerically_and_unrelated_tags_are_ignored(self):
        releases = [
            {"tag_name": "unrelated", "draft": False, "prerelease": False},
            {"tag_name": "v4.9.0", "draft": False, "prerelease": False},
        ]
        self.assertTrue(module.is_new_release("4.10.0", releases))
        self.assertFalse(module.is_new_release("4.9.0", releases))

    def test_invalid_version_cannot_enter_shell_outputs(self):
        for value in ["$(id)", "4.0.0\ntag=other", "4.0.0-beta.1"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                module.version_tuple(value)

    def test_script_flattens_release_pages_and_appends_all_workflow_outputs(self):
        for version, expected_new in [("4.0.0", "false"), ("4.1.0", "true")]:
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "Cargo.toml").write_text(f'[package]\nversion = "{version}"\n')
                output = root / "output"
                output.write_text("existing=value\n")
                pages = [
                    [{"tag_name": "v5.0.0-rc.1", "draft": False, "prerelease": True}],
                    [{"tag_name": "v4.0.0", "draft": False, "prerelease": False}],
                ]
                with contextlib.chdir(root), contextlib.redirect_stdout(io.StringIO()), mock.patch.dict(
                    os.environ, {"GITHUB_REPOSITORY": "test/repo", "GITHUB_OUTPUT": str(output)}
                ), mock.patch("subprocess.check_output", return_value=json.dumps(pages)) as gh:
                    runpy.run_path(str(SCRIPT), run_name="__main__")
                gh.assert_called_once_with([
                    "gh", "api", "repos/test/repo/releases", "--paginate", "--slurp"
                ], text=True)
                self.assertEqual(output.read_text(), (
                    f"existing=value\nversion={version}\ntag=v{version}\nis_new={expected_new}\n"
                ))

    def test_script_does_not_write_publish_outputs_when_release_lookup_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nversion = "4.0.0"\n')
            output = root / "output"
            with contextlib.chdir(root), mock.patch.dict(
                os.environ, {"GITHUB_REPOSITORY": "test/repo", "GITHUB_OUTPUT": str(output)}
            ), mock.patch("subprocess.check_output", side_effect=subprocess.CalledProcessError(1, "gh")):
                with self.assertRaises(subprocess.CalledProcessError):
                    runpy.run_path(str(SCRIPT), run_name="__main__")
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
