import os

PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))

DATASET_DIR = os.path.join(PROJECT_ROOT, "data")
OUTPUTS_DIR = os.path.join(PROJECT_ROOT, "outputs")

DEFAULT_EMBEDDING_MODEL = "baai/bge-m3"
DEFAULT_DIMENSIONS = 1024
DEFAULT_API_KEY_ENV = "OPENROUTER_API_KEY"

CARGO_BIN = ["cargo", "run", "-p", "memory-bench", "--"]
