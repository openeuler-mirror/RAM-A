import argparse
import sys
from pathlib import Path

EVALUATION_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(EVALUATION_ROOT))

from common.run_artifacts import write_run_meta


def main():
    parser = argparse.ArgumentParser(description="Write LoCoMo run metadata.")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--backend", required=True)
    parser.add_argument("--phase", required=True)
    parser.add_argument("--top-k", type=int, required=True)
    parser.add_argument("--run-dir", required=True)
    args = parser.parse_args()

    write_run_meta(
        args.output,
        dataset="locomo",
        backend=args.backend,
        phase=args.phase,
        dataset_path=args.dataset,
        run_dir=args.run_dir,
        top_k=args.top_k,
    )
    print(f"LoCoMo run metadata saved to {args.output}")


if __name__ == "__main__":
    main()
