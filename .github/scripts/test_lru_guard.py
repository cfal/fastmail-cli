import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check-lru-cache.sh")
BASH = shutil.which("bash")


class LruGuardTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def repository(self, source):
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "src").mkdir()
        (self.root / "src" / "lib.rs").write_text(source)
        subprocess.run(["git", "add", "src"], cwd=self.root, check=True)

    def run_guard(self, env=None):
        return subprocess.run(
            [BASH, str(SCRIPT.resolve())],
            cwd=self.root,
            env=env,
            capture_output=True,
            text=True,
        )

    def test_unaffected_source_passes_with_only_git_on_path(self):
        self.repository("type RecordCache = HashMapCache;\n")
        tools = self.root / "bin"
        tools.mkdir()
        (tools / "git").symlink_to(shutil.which("git"))
        result = self.run_guard({**os.environ, "PATH": str(tools)})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_lru_cache_reference_fails(self):
        self.repository("type RecordCache = LruCache;\n")
        result = self.run_guard()
        self.assertEqual(result.returncode, 1)
        self.assertIn("src/lib.rs:1:type RecordCache = LruCache;", result.stdout)
        self.assertIn("Reassess the lru advisory exception", result.stdout)

    def test_search_error_fails(self):
        result = self.run_guard()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a git repository", result.stderr)

    def test_missing_git_fails(self):
        self.repository("type RecordCache = HashMapCache;\n")
        result = self.run_guard({**os.environ, "PATH": ""})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("git", result.stderr)


if __name__ == "__main__":
    unittest.main()
