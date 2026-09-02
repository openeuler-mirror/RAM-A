from pathlib import Path
import json
import unittest


ROOT = Path(__file__).resolve().parents[1]


class Arm64ImageAssetsTest(unittest.TestCase):
    def test_required_assets_exist(self):
        required = {
            "Dockerfile",
            ".dockerignore",
            "config/ram-a-mem.json",
            "config/xiaoo.toml",
            "config/xiaoo.mcp.json",
            "scripts/start-ram-a.sh",
            "scripts/start-xiaoo.sh",
            "scripts/start-all.sh",
            "scripts/healthcheck.sh",
            "scripts/mcp.sh",
            "scripts/verify-ingest.sh",
            "scripts/rebuild-ram-a.sh",
            "scripts/image-info.sh",
            "build-image.ps1",
            "run-image.ps1",
            "verify-image.ps1",
        }
        missing = sorted(str(path) for path in required if not (ROOT / path).is_file())
        self.assertEqual([], missing)

    def test_dockerfile_builds_pr18_for_arm64(self):
        text = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        self.assertIn("openeuler/openeuler:24.03-lts-sp3", text)
        self.assertIn("refs/merge-requests/18/head", text)
        self.assertIn("cargo build --release --locked -p memory-mcp", text)
        self.assertIn("ARG GLM_CODING_TOKEN", text)
        self.assertIn("ARG OPENROUTER_API_KEY", text)

    def test_ram_a_config_uses_hash(self):
        config = json.loads((ROOT / "config/ram-a-mem.json").read_text(encoding="utf-8"))
        self.assertEqual("hash", config["providers"]["embedding_provider"])
        self.assertEqual(1024, config["providers"]["embedding_dimensions"])
        self.assertEqual("GLM-5.2", config["providers"]["extractor_model"])
        self.assertFalse(config["features"]["graph_memory"]["enabled"])
        self.assertFalse(config["retrieval"]["rerank"]["enabled"])

    def test_repository_assets_do_not_contain_real_tokens(self):
        forbidden = ("f12f" + "159980484722", "sk-or-v1-" + "87ad6b14")
        for path in ROOT.rglob("*"):
            if path.is_file() and path.suffix not in {".pyc", ".rpm"}:
                text = path.read_text(encoding="utf-8", errors="ignore")
                for prefix in forbidden:
                    self.assertNotIn(prefix, text, str(path))


if __name__ == "__main__":
    unittest.main()
