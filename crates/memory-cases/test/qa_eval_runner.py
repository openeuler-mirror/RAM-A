#!/usr/bin/env python3
import argparse
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def load_cases(path: str) -> list[dict]:
    # case 文件固定使用缩进 JSON 数组，便于阅读、review 和批量维护。
    text = Path(path).read_text(encoding="utf-8")
    raw_cases = json.loads(text)
    if not isinstance(raw_cases, list):
        raise ValueError(f"case file must be a JSON array: {path}")
    cases = []
    for index, case in enumerate(raw_cases, start=1):
        if not isinstance(case, dict):
            raise ValueError(f"case #{index} must be a JSON object: {path}")
        case.setdefault("id", f"case-{index}")
        cases.append(case)
    return cases


def command_count(args: argparse.Namespace) -> int:
    # 给 shell 脚本做前置校验用：确认 case 文件至少有一条有效用例。
    print(len(load_cases(args.cases_file)))
    return 0


def command_solution_count(args: argparse.Namespace) -> int:
    # 声明 required_solution_terms 的用例专门覆盖“命中文档后带回解决方案”的回归。
    print(sum(1 for case in load_cases(args.cases_file) if strings(case.get("required_solution_terms"))))
    return 0


def command_sources(args: argparse.Namespace) -> int:
    # shell 会把这些 expected_sources 强制加入上传列表。
    # 即使本地设置了 MEMORY_CASES_QA_MAX_DOCS，也不能漏掉 case 明确依赖的文档。
    seen = set()
    for case in load_cases(args.cases_file):
        for source in expected_sources(case):
            if source and source not in seen:
                seen.add(source)
                print(source)
    return 0


def post_json(base_url: str, path: str, payload: dict) -> dict:
    # 不走系统代理，避免本地 127.0.0.1 API 请求被代理环境变量干扰。
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        f"{base_url}{path}",
        data=body,
        headers={"content-type": "application/json"},
        method="POST",
    )
    try:
        with OPENER.open(request, timeout=60) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        error_body = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"HTTP {error.code} for {path}: {error_body}") from error


def strings(value) -> list[str]:
    # case 字段允许写成字符串或数组；统一成 list，后续校验逻辑就简单一些。
    if value is None:
        return []
    if isinstance(value, str):
        return [value]
    return [str(item) for item in value]


def text_field(value) -> str:
    # question/standard_answer 可以写成字符串数组，便于长文本在 JSON 里分行维护。
    if value is None:
        return ""
    if isinstance(value, list):
        return "\n".join(str(item) for item in value)
    return str(value)


def missing_terms(text: str, terms: list[str]) -> list[str]:
    # 关键词校验使用 casefold，兼容 ASCII 大小写差异；中文不受影响。
    folded = text.casefold()
    return [term for term in terms if term.casefold() not in folded]


def source_label(chunk: dict) -> str:
    return chunk.get("source_name") or chunk.get("source_path") or chunk.get("document_id") or ""


def source_matches(chunk: dict, expected_source: str) -> bool:
    # 服务可能把来源放在 source_name、source_path 或 document_id 中。
    # 这里只做包含匹配，方便 expected_source 写文件名而不是绝对路径。
    if not expected_source:
        return True
    labels = [
        chunk.get("source_name") or "",
        chunk.get("source_path") or "",
        chunk.get("document_id") or "",
    ]
    return any(expected_source in label for label in labels)


def source_hit(chunks: list[dict], expected_source: str) -> bool:
    return any(source_matches(chunk, expected_source) for chunk in chunks)


def unexpected_sources(chunks: list[dict], expected: list[str]) -> list[str]:
    sources = []
    for chunk in chunks:
        source = source_label(chunk)
        if not source:
            continue
        if any(source_matches(chunk, expected_source) for expected_source in expected):
            continue
        if source not in sources:
            sources.append(source)
    return sources


def expected_sources(case: dict) -> list[str]:
    # expected_sources 表示期望召回的一篇或多篇文档。
    if case.get("expect_no_hits"):
        return []
    return strings(case.get("expected_sources"))


def preview(text: str, max_chars: int = 220) -> str:
    # 报告只保存片段预览，避免 QA 报告被长文档内容撑得太大。
    if len(text) <= max_chars:
        return text
    return text[:max_chars] + "..."


def evaluate_case(case: dict, base_url: str, dataset_id: str) -> dict:
    # 一个 case 的核心字段：
    # - question：用户问题。
    # - expected_sources：期望被召回的一篇或多篇文档。
    # - expect_no_hits：期望没有任何关联文档。
    # - required_answer_terms：期望 chat answer 摘要里出现的词。
    # - required_reference_terms：期望全部 references 中出现的词。
    # - required_solution_terms：期望来自 expected_sources 的 references 中出现的解决方案词。
    case_id = case["id"]
    question = text_field(case["question"])
    top_k = int(case.get("top_k", 5))
    expected = expected_sources(case)
    expect_no_hits = bool(case.get("expect_no_hits", False))
    answer_terms = strings(case.get("required_answer_terms", []))
    reference_terms = strings(case.get("required_reference_terms", []))
    solution_terms = strings(case.get("required_solution_terms", []))

    case_result = {
        "id": case_id,
        "question": question,
        "standard_answer": text_field(case.get("standard_answer", "")),
        "expected_sources": expected,
        "expect_no_hits": expect_no_hits,
        "has_solution_terms": bool(solution_terms),
        "top_k": top_k,
    }

    try:
        # search 和 chat 都要测：
        # search 验证底层检索接口能命中文档；
        # chat 验证对外问答接口最终给出的 references 也没有丢掉目标文档。
        search = post_json(
            base_url,
            f"/api/v1/datasets/{dataset_id}/search",
            {"query": question, "top_k": top_k},
        )
        chat = post_json(
            base_url,
            "/api/v1/chat/completions",
            {"dataset_id": dataset_id, "question": question, "top_k": top_k},
        )
        search_chunks = search.get("chunks", [])
        references = chat.get("references", [])
        answer = chat.get("answer", "")
        reference_text = "\n".join(str(chunk.get("content", "")) for chunk in references)

        if expect_no_hits:
            checks = {
                # 无关联问题应该没有 search 命中，也不应该给 chat references。
                "search_has_no_hits": not search_chunks,
                "chat_has_no_references": not references,
                "answer_reports_no_hits": "No relevant content was found" in answer,
            }
            case_result.update(
                {
                    "passed": all(checks.values()),
                    "checks": checks,
                    "actual_answer": answer,
                    "search_hits": summarize_chunks(search_chunks),
                    "references": summarize_chunks(references),
                }
            )
            return case_result

        # solution_terms 最关心“命中同一文档后是否把解决方案 chunk 带回来”。
        # 所以只在 expected_sources 的 references 里找，防止其它文档里的同名配置项让测试误通过。
        expected_source_reference_text = "\n".join(
            str(chunk.get("content", ""))
            for chunk in references
            if any(source_matches(chunk, source) for source in expected)
        )

        missing_answer = missing_terms(answer, answer_terms)
        missing_reference = missing_terms(reference_text, reference_terms)
        missing_solution = missing_terms(expected_source_reference_text, solution_terms)
        missing_search_sources = [source for source in expected if not source_hit(search_chunks, source)]
        missing_chat_sources = [source for source in expected if not source_hit(references, source)]
        unexpected_search_sources = unexpected_sources(search_chunks, expected)
        unexpected_reference_sources = unexpected_sources(references, expected)
        checks = {
            # /search 必须能召回全部预期文档。
            "search_expected_sources_hit": not missing_search_sources,
            # /chat/completions 的 references 也必须保留全部预期文档。
            "chat_expected_sources_hit": not missing_chat_sources,
            # answer 是当前服务拼出来的摘要，检查它是否覆盖关键答案词。
            "answer_required_terms_hit": not missing_answer,
            # references 要覆盖根因、日志、上下文等证据词。
            "reference_required_terms_hit": not missing_reference,
            # 解决方案词必须来自预期文档的引用片段。
            "solution_terms_from_expected_source_hit": not missing_solution,
            # references 不应混入 expected_sources 之外的文档。
            "unexpected_sources_absent": not unexpected_reference_sources,
        }
        case_result.update(
            {
                "passed": all(checks.values()),
                "checks": checks,
                "actual_answer": answer,
                "missing_search_sources": missing_search_sources,
                "missing_chat_sources": missing_chat_sources,
                "unexpected_search_sources": unexpected_search_sources,
                "unexpected_reference_sources": unexpected_reference_sources,
                "missing_answer_terms": missing_answer,
                "missing_reference_terms": missing_reference,
                "missing_solution_terms": missing_solution,
                "search_hits": summarize_chunks(search_chunks),
                "references": summarize_chunks(references),
            }
        )
    except Exception as error:
        case_result.update({"passed": False, "error": str(error)})

    return case_result


def summarize_chunks(chunks: list[dict]) -> list[dict]:
    # 报告里保留命中来源、chunk_id、分数和内容预览，方便失败后快速判断错召/漏召。
    return [
        {
            "source": source_label(chunk),
            "document_id": chunk.get("document_id", ""),
            "chunk_id": chunk.get("chunk_id", ""),
            "score": chunk.get("score"),
            "content_preview": preview(str(chunk.get("content", ""))),
        }
        for chunk in chunks
    ]


def write_report(report_path: str, report: dict) -> None:
    # JSON 报告保留完整检查项和命中片段摘要，给 CI 或人工排查使用。
    Path(report_path).parent.mkdir(parents=True, exist_ok=True)
    with open(report_path, "w", encoding="utf-8") as handle:
        json.dump(report, handle, ensure_ascii=False, indent=2)
        handle.write("\n")


def print_result_summary(results: list[dict], report_path: str) -> None:
    # 控制台只打印紧凑结果；详细命中片段写进 report 文件。
    passed_count = sum(1 for item in results if item.get("passed"))
    solution_total = sum(1 for item in results if item.get("has_solution_terms"))
    solution_passed = sum(1 for item in results if item.get("has_solution_terms") and item.get("passed"))
    no_hit_total = sum(1 for item in results if item.get("expect_no_hits"))
    no_hit_passed = sum(1 for item in results if item.get("expect_no_hits") and item.get("passed"))

    for item in results:
        status = "PASS" if item.get("passed") else "FAIL"
        print(f"[{status}] {item['id']} - {item['question']}")
        if item.get("passed"):
            continue
        if item.get("error"):
            print(f"  error: {item['error']}")
        if item.get("missing_search_sources"):
            print("  missing search sources: " + ", ".join(item["missing_search_sources"]))
        if item.get("missing_chat_sources"):
            print("  missing chat sources: " + ", ".join(item["missing_chat_sources"]))
        if item.get("unexpected_reference_sources"):
            print("  unexpected reference sources: " + ", ".join(item["unexpected_reference_sources"]))
        print_missing_terms(item)
        checks = item.get("checks") or {}
        failed_checks = [name for name, ok in checks.items() if not ok]
        if failed_checks:
            print("  failed checks: " + ", ".join(failed_checks))

    print(f"qa eval result: {passed_count}/{len(results)} passed")
    print(f"solution terms result: {solution_passed}/{solution_total} passed")
    print(f"no-hit result: {no_hit_passed}/{no_hit_total} passed")
    print(f"report: {report_path}")


def print_missing_terms(item: dict) -> None:
    # 失败时优先展示缺失词，比直接翻完整 JSON 报告快很多。
    labels = [
        ("missing_answer_terms", "missing answer terms"),
        ("missing_reference_terms", "missing reference terms"),
        ("missing_solution_terms", "missing solution terms"),
    ]
    for key, label in labels:
        if item.get(key):
            print(f"  {label}: " + ", ".join(item[key]))


def command_run(args: argparse.Namespace) -> int:
    # run 是真正执行 QA 的子命令：逐条请求 API、汇总通过率、写报告并设置退出码。
    cases = load_cases(args.cases_file)
    results = [evaluate_case(case, args.base_url, args.dataset_id) for case in cases]

    passed_count = sum(1 for item in results if item.get("passed"))
    failed_count = len(results) - passed_count
    solution_total = sum(1 for item in results if item.get("has_solution_terms"))
    solution_passed = sum(1 for item in results if item.get("has_solution_terms") and item.get("passed"))
    no_hit_total = sum(1 for item in results if item.get("expect_no_hits"))
    no_hit_passed = sum(1 for item in results if item.get("expect_no_hits") and item.get("passed"))
    report = {
        "dataset_id": args.dataset_id,
        "case_file": args.cases_file,
        "summary": {
            "total": len(results),
            "passed": passed_count,
            "failed": failed_count,
            "solution_terms_total": solution_total,
            "solution_terms_passed": solution_passed,
            "no_hit_total": no_hit_total,
            "no_hit_passed": no_hit_passed,
        },
        "results": results,
    }

    write_report(args.report_file, report)
    print_result_summary(results, args.report_file)
    return 1 if failed_count else 0


def build_parser() -> argparse.ArgumentParser:
    # 这个 runner 同时服务 shell 的准备阶段和执行阶段：
    # count/solution-count/sources 用于 shell 前置校验，run 用于真正跑 QA。
    parser = argparse.ArgumentParser(description="Run memory-cases QA evaluation helpers.")
    subparsers = parser.add_subparsers(dest="command", required=True)

    count = subparsers.add_parser("count")
    count.add_argument("cases_file")
    count.set_defaults(func=command_count)

    solution_count = subparsers.add_parser("solution-count")
    solution_count.add_argument("cases_file")
    solution_count.set_defaults(func=command_solution_count)

    sources = subparsers.add_parser("sources")
    sources.add_argument("cases_file")
    sources.set_defaults(func=command_sources)

    run = subparsers.add_parser("run")
    run.add_argument("cases_file")
    run.add_argument("report_file")
    run.add_argument("base_url")
    run.add_argument("dataset_id")
    run.set_defaults(func=command_run)

    return parser


def main(argv: list[str]) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
