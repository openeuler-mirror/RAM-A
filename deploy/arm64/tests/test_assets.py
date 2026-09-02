from pathlib import Path
import json
import re
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
            "install-arm64-emulation.ps1",
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

    def test_dockerfile_does_not_expand_credentials_in_run_steps(self):
        text = (ROOT / "Dockerfile").read_text(encoding="utf-8")
        for line in text.splitlines():
            if line.lstrip().startswith("RUN "):
                self.assertNotIn("GLM_CODING_TOKEN", line)
                self.assertNotIn("OPENROUTER_API_KEY", line)

    def test_ram_a_config_uses_hash(self):
        config = json.loads((ROOT / "config/ram-a-mem.json").read_text(encoding="utf-8"))
        self.assertEqual("127.0.0.1", config["http"]["bind_address"])
        self.assertEqual("hash", config["providers"]["embedding_provider"])
        self.assertEqual(1024, config["providers"]["embedding_dimensions"])
        self.assertEqual("GLM-5.2", config["providers"]["extractor_model"])
        self.assertFalse(config["features"]["graph_memory"]["enabled"])
        self.assertFalse(config["retrieval"]["rerank"]["enabled"])

    def test_arm64_emulation_installer_is_pinned_and_preserves_argv0(self):
        text = (ROOT / "install-arm64-emulation.ps1").read_text(encoding="utf-8")
        self.assertIn("deploy/v10.2.3-68", text)
        self.assertIn("9f955e980acb29986365766db7d36383a1dbbf2159f30b532a74474f6f5dd75d", text)
        self.assertIn("8e7d8f4c0c7809fc3fea0085199fd6b16f671e7c73d9bf6bec711e1cb535920a", text)
        self.assertIn("QEMU_PRESERVE_ARGV0=1", text)
        self.assertIn("aarch64", text)

    def test_repository_assets_do_not_contain_real_tokens(self):
        forbidden = ("f12f" + "159980484722", "sk-or-v1-" + "87ad6b14")
        for path in ROOT.rglob("*"):
            if path.is_file() and path.suffix not in {".pyc", ".rpm"}:
                text = path.read_text(encoding="utf-8", errors="ignore")
                for prefix in forbidden:
                    self.assertNotIn(prefix, text, str(path))

    def test_glm_smoke_allows_reasoning_responses(self):
        text = (ROOT / "scripts/verify-ingest.sh").read_text(encoding="utf-8")
        self.assertIn("max_tokens:64", text)
        self.assertIn("reasoning_content", text)

    def test_powershell_variables_before_colons_are_delimited(self):
        invalid_reference = re.compile(
            r"\$(?!(?:env|global|script|local|private):)[A-Za-z_][A-Za-z0-9_]*:"
        )
        for path in ROOT.glob("*.ps1"):
            text = path.read_text(encoding="utf-8")
            self.assertIsNone(invalid_reference.search(text), str(path))


if __name__ == "__main__":
    unittest.main()
