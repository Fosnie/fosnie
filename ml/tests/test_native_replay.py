# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import json
import pytest
from app import rag_ctx,gemini_adapter
from app.native_replay import bind,get,responses_input,responses_state
from test_inference_target import clean_context


def context():rag_ctx.set_overrides({"llm_model":"opaque-model","routing_provider_id":"provider-fixture"})


def test_responses_replays_native_items_and_serializes_tool_results():
    context()
    native=[{"type":"reasoning","id":"rs_fixture","summary":[{"text":"must never be forwarded"}],"encrypted_content":"opaque-encrypted"},
        {"type":"function_call","id":"fc_fixture","call_id":"call_fixture","name":"web_search","arguments":'{"query": "public", "depth": "quick"}'}]
    metadata=responses_state(native)
    message={"role":"assistant","content":"","tool_calls":[{"id":"call_fixture","function":{"name":"web_search","arguments":'{"depth":"quick","query":"public"}'},"native_replay":metadata}]}
    output=responses_input([message,{"role":"tool","tool_call_id":"call_fixture","content":{"result":"public evidence"}}])
    assert [v["type"] for v in output]==["reasoning","function_call","function_call_output"]
    assert output[0]["summary"]==[]
    assert output[1]["call_id"]=="call_fixture"
    assert json.loads(output[2]["output"])=={"result":"public evidence"}
    assert "must never be forwarded" not in json.dumps(output)


def test_gemini_signature_and_native_id_survive_tool_round_trip():
    context()
    part={"functionCall":{"id":"native-id","name":"web_search","args":{"query":"public"}},"thoughtSignature":"opaque-signature"}
    content,calls=gemini_adapter._parse_parts([{"thought":True,"text":"must stay private"},part])
    assert content=="" and calls[0]["id"]=="native-id"
    chat={"role":"assistant","content":"","tool_calls":[{"id":calls[0]["id"],"function":{"name":calls[0]["name"],"arguments":json.dumps(calls[0]["arguments"])},"native_replay":calls[0]["native_replay"]}]}
    _,translated=gemini_adapter._translate([chat,{"role":"tool","tool_call_id":"native-id","content":"public result"}])
    assert translated[0]["parts"][0]==part
    assert translated[1]["parts"][0]["functionResponse"]["id"]=="native-id"
    assert "must stay private" not in json.dumps(translated)


def test_replay_does_not_cross_model_or_provider_and_rejects_call_tampering():
    context();metadata=bind("gemini",{"part":{}})
    rag_ctx.set_overrides({"llm_model":"another-model","routing_provider_id":"provider-fixture"})
    assert get({"native_replay":metadata},"gemini") is None
    rag_ctx.set_overrides({"llm_model":"opaque-model","routing_provider_id":"another-provider"})
    assert get({"native_replay":metadata},"gemini") is None
    context()
    metadata=responses_state([{"type":"function_call","call_id":"x","name":"web_search","arguments":"{}"}])
    with pytest.raises(ValueError):responses_input([{"role":"assistant","tool_calls":[{"id":"y","function":{"name":"web_search","arguments":"{}"},"native_replay":metadata}]}])
    with pytest.raises(ValueError):bind("gemini",{"part":"x"*70000})
