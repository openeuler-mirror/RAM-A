"""Internal preservation of provider-owned KV fields. Never invent request IDs."""
from collections.abc import Mapping

from langchain_openai import ChatOpenAI


class _KvResponseChatOpenAI(ChatOpenAI):
    """Keep KV fields during Chat Completions response conversion."""

    def _create_chat_result(self, response, generation_info=None):
        result = super()._create_chat_result(response, generation_info)
        transfer = (response.get("kv_transfer_params") if isinstance(response, dict)
                    else getattr(response, "kv_transfer_params", None))
        if transfer is not None:
            for generation in result.generations:
                generation.message.response_metadata["kv_transfer_params"] = transfer
        return result


def preserve_kv_response(request):
    """Adapt standard ChatOpenAI on a copy, retaining its clients and settings.

    Subclasses own their conversion behavior and are left intact. No global
    monkey patch, request modification or new HTTP connection is involved.
    """
    model = request.model
    if type(model) is ChatOpenAI:
        model = _KvResponseChatOpenAI.model_construct(
            _fields_set=model.model_fields_set, **model.__dict__
        )
        return request.override(model=model)
    return request


def model_request_id(request):
    """Read the actual outgoing field; do not confuse graph run IDs with it."""
    settings = request.model_settings or {}
    # Per-call extra_body replaces model-level extra_body in ChatOpenAI.
    extra = settings.get("extra_body", getattr(request.model, "extra_body", None))
    value = extra.get("request_id") if isinstance(extra, Mapping) else None
    if value is None:
        value = settings.get("request_id")
    return value if isinstance(value, str) and value.strip() else None


def cache_hashes_from_response(_context, response):
    for message in response.result:
        transfer = message.response_metadata.get("kv_transfer_params")
        if transfer is not None:
            if not isinstance(transfer, Mapping):
                raise ValueError("kv_transfer_params must be an object")
            return transfer.get("ucm_block_ids")
    return None
