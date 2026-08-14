import argparse
from pathlib import Path

TECHNIQUES = ["mem0"]
METHODS = ["add", "search"]


def load_mem0_classes():
    from locomo.backends.mem0.add import MemoryADD
    from locomo.backends.mem0.search import MemorySearch

    return MemoryADD, MemorySearch


def main():
    parser = argparse.ArgumentParser(description="Run memory experiments")
    parser.add_argument("--technique-type", choices=TECHNIQUES, default="mem0", help="Memory technique to use")
    parser.add_argument("--method", choices=METHODS, default="add", help="Method to use")
    parser.add_argument("--dataset", type=Path, required=True, help="LoCoMo dataset file.")
    parser.add_argument("--output", type=Path, help="Output JSON file path for search results")
    parser.add_argument("--storage-dir", type=Path, required=True, help="Directory for mem0 storage files.")
    parser.add_argument("--top-k", type=int, default=30, help="Number of top memories to retrieve")
    parser.add_argument("--workers", type=int, default=4, help="Number of worker threads for mem0 add")
    parser.add_argument("--debug", action="store_true", help="Print detailed diagnostics while adding memories")
    parser.add_argument(
        "--no-infer",
        dest="infer",
        action="store_false",
        default=True,
        help="Disable mem0 AI inference when storing memories (enabled by default)",
    )

    args = parser.parse_args()
    if args.method == "search" and args.output is None:
        parser.error("--output is required when --method search")

    print(f"Running experiments with technique: {args.technique_type}")

    if args.technique_type == "mem0":
        MemoryADD, MemorySearch = load_mem0_classes()
        if args.method == "add":
            memory_manager = MemoryADD(
                data_path=args.dataset,
                storage_dir=args.storage_dir,
                debug=args.debug,
                infer=args.infer,
            )
            try:
                memory_manager.process_all_conversations(max_workers=args.workers)
            finally:
                memory_manager.close()
        elif args.method == "search":
            output_file_path = args.output
            if output_file_path.is_dir():
                raise IsADirectoryError(f"Output path must be a file, but is a directory: {output_file_path}")
            memory_searcher = MemorySearch(output_file_path, args.storage_dir, args.top_k)
            try:
                memory_searcher.process_data_file(args.dataset)
            finally:
                memory_searcher.close()
        else:
            raise ValueError(f"Invalid method: {args.method}")
    else:
        raise ValueError(f"Invalid technique type: {args.technique_type}")


if __name__ == "__main__":
    main()
