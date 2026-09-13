import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("release_version", Path(__file__).with_name("release-version.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


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
        self.assertTrue(module.is_new_release("4.0.0", []))

    def test_invalid_version_cannot_enter_shell_outputs(self):
        for value in ["$(id)", "4.0.0\ntag=other", "4.0.0-beta.1"]:
            with self.assertRaises(ValueError):
                module.version_tuple(value)


if __name__ == "__main__":
    unittest.main()
