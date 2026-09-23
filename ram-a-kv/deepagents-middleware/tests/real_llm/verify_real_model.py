#!/usr/bin/env python3
"""Real end-to-end verification: DeepAgents -> ram_a_middleware -> RAM-A (ram-a-kv @ 6990)
with a REAL model service at http://127.0.0.1:8000/v1/chat/completions.

Usage (with ram-a-sdk-main venv activated, from any directory):
    export MODEL_BASE_URL=http://127.0.0.1:8000/v1
    export RAM_A_URL=http://127.0.0.1:6990
    export NO_PROXY=127.0.0.1,localhost
    python /home/xxxx/verify_real_model.py
"""
import json
import os
import sys

import httpx
from deepagents import create_deep_agent
from langchain_openai import ChatOpenAI
from ram_a_middleware.sdk import RamAClient
from ram_a_middleware.deepagents import RamAKvMiddleware


def main() -> int:
    base_url = os.environ.get("MODEL_BASE_URL", "http://127.0.0.1:8000/v1")
    ram_a_url = os.environ.get("RAM_A_URL", "http://127.0.0.1:6990")

    # Resolve the served model name from /v1/models.
    with httpx.Client(timeout=10, trust_env=False) as c:
        resp = c.get(f"{base_url}/models")
        resp.raise_for_status()
        model_name = resp.json()["data"][0]["id"]
    print(f"[setup] model={model_name} base_url={base_url} ram_a={ram_a_url}")

    model = ChatOpenAI(
        model=model_name,
        base_url=base_url,
        api_key=os.getenv("MODEL_API_KEY", "local-test"),
        use_responses_api=False,   # KV transfer: non-streaming Chat Completions only
        streaming=False,
        timeout=120,
    )

    session_id = "verify-real-1"
    try:
        with RamAClient(ram_a_url, auth_token=os.getenv("RAM_A_AUTH_TOKEN") or None, timeout=2) as client:
            agent = create_deep_agent(model=model, middleware=[RamAKvMiddleware(client)])
            config = {"configurable": {"thread_id": session_id}}
            messages = []
            # Turn 1 should write cache identifiers at turn_end; turn 2 should
            # trigger turn_start prefetch in the same session.
            for text in ("你好,请简短回答:1+1等于几?", "请继续简短回答:再乘以3等于多少?"):
                messages.append({"role": "user", "content": text})
                result = agent.invoke({"messages": messages}, config=config)
                content = result["messages"][-1].content
                print(f"[turn] user={text!r} assistant={content!r}")
                messages = result["messages"]
        print("AGENT_RUN_OK")
    finally:
        model.root_client.close()

    # Inspect the session mapping recorded by RAM-A.
    with httpx.Client(timeout=5, trust_env=False) as c:
        r = c.post(
            f"{ram_a_url}/event",
            json={"type": "session_map", "session_id": f"deepagents:{session_id}"},
        )
        print("=== session_map ===")
        print(json.dumps(r.json(), ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
