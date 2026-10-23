# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
from uuid import uuid4
import pytest
from app.variant_generation import apply, request
from app.inference_target import InferenceTarget, InferenceEnvelope
from app import rag_ctx


def target(version=2):
    result=dict(schema_version=version,provider_id=str(uuid4()),provider_revision=4,profile_revision=3,
                adapter="openai_chat",base_url="http://127.0.0.1:1/v1",model="fixture",api_key="synthetic",effort="low")
    if version==2:
        result.update(variant=dict(variant=dict(id=str(uuid4()),revision=5),model_config_revision=2,behaviour_sha256="a"*64),generation={})
    return result


@pytest.mark.parametrize("adapter,body,key",[("openai_chat",{"max_completion_tokens":256},"max_completion_tokens"),
    ("openai_responses",{"max_output_tokens":256},"max_output_tokens"),("anthropic",{"max_tokens":256},"max_tokens"),
    ("gemini",{"generationConfig":{"maxOutputTokens":256}},"maxOutputTokens")])
def test_pinned_generation_uses_native_names_and_preserves_call_limit(adapter,body,key):
    g=dict(max_output_tokens=128,temperature_milli=0,top_p_milli=900)
    effective=apply(body,adapter,g)
    settings=effective.get("generationConfig",effective)
    assert settings[key]==128
    assert settings["temperature"]==0
    assert settings["topP" if adapter=="gemini" else "top_p"]==0.9
    assert apply({key:64} if adapter!="gemini" else {"generationConfig":{key:64}},adapter,g).get("generationConfig",{}).get(key,64)==64
    assert body.get("generationConfig",body)[key]==256, "immutable source payload"


@pytest.mark.parametrize("adapter",["openai_chat","openai_responses"])
def test_reasoning_budget_has_no_invented_openai_mapping(adapter):
    with pytest.raises(ValueError):apply({"max_tokens":4096},adapter,{"max_reasoning_tokens":1024})


def test_native_reasoning_budget_and_mode_conflict():
    assert apply({"max_tokens":4096},"anthropic",{"max_reasoning_tokens":2048})["thinking"]=={"type":"enabled","budget_tokens":2048}
    with pytest.raises(ValueError):apply({"max_tokens":1024},"anthropic",{"max_reasoning_tokens":2048})
    with pytest.raises(ValueError):apply({"generationConfig":{"thinkingConfig":{"thinkingLevel":"HIGH"}}},"gemini",{"max_reasoning_tokens":1024})
    assert apply({"generationConfig":{}},"gemini",{"max_reasoning_tokens":1024})["generationConfig"]["thinkingConfig"]["thinkingBudget"]==1024


def test_v2_requires_complete_strict_binding_and_v1_never_accepts_it():
    value=target();InferenceTarget.model_validate(value)
    for key in ("variant","generation"):
        bad=dict(value);bad.pop(key)
        with pytest.raises(ValueError):InferenceTarget.model_validate(bad)
    value["schema_version"]=1
    with pytest.raises(ValueError):InferenceTarget.model_validate(value)
    for key in ("variant","generation"):
        v1=target(1);v1[key]=None
        with pytest.raises(ValueError):InferenceTarget.model_validate(v1)
    bad=target();bad["variant"]["variant"]["revision"]=0
    with pytest.raises(ValueError):InferenceTarget.model_validate(bad)
    bad=target();bad["variant"]["variant"]["extra"]=True
    with pytest.raises(ValueError):InferenceTarget.model_validate(bad)
    with pytest.raises(ValueError):InferenceEnvelope.model_validate(dict(schema_version=1,decision_id=str(uuid4()),purpose="primary",assignments={"primary":target()}))


def test_mixed_fixed_service_does_not_receive_variant_settings():
    with rag_ctx.scoped_overrides({"routing_generation":{"temperature_milli":250},"llm_adapter":"openai_chat"}):
        body={"json":{"model":"embedding"}}
        assert request("http://127.0.0.1/v1/embeddings",body)==body
        assert request("http://127.0.0.1/v1/chat/completions",{"json":{"max_tokens":128}})["json"]["temperature"]==0.25


@pytest.mark.parametrize("model",["gemma-4-26b-a4b-it","models/gemma-4-31b-it","gemini-4-argon"])
def test_provider_default_level_models_cannot_inject_numeric_reasoning_budget(model):
    with rag_ctx.scoped_overrides({"routing_generation":{"max_reasoning_tokens":1024},"llm_adapter":"gemini","llm_model":model}):
        with pytest.raises(ValueError,match="not a numeric reasoning budget"):
            request("https://generativelanguage.googleapis.com/v1beta/models/"+model+":generateContent",{"json":{"generationConfig":{"maxOutputTokens":4096}}})
