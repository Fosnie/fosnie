# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Bounded opaque provider state. Never includes plaintext thought summaries."""
import json
from .rag_ctx import cfg


def bind(adapter,data):
    value={"schema_version":1,"adapter":adapter,"model":cfg("llm_model",None),
        "provider_id":cfg("routing_provider_id",None),"data":data}
    if len(json.dumps(value,ensure_ascii=False).encode())>65536:
        raise ValueError("native replay metadata exceeds bound")
    return value


def get(call,adapter):
    value=call.get("native_replay")
    if value is None:return None
    if not isinstance(value,dict) or len(json.dumps(value,ensure_ascii=False).encode())>65536:
        raise ValueError("invalid native replay metadata")
    if (value.get("schema_version")!=1 or value.get("adapter")!=adapter
        or value.get("model")!=cfg("llm_model",None)
        or value.get("provider_id")!=cfg("routing_provider_id",None)):
        return None
    return value.get("data")


def responses_input(messages):
    out=[]
    for message in messages:
        role=message.get("role");calls=message.get("tool_calls") or []
        if role=="tool":
            content=message.get("content","")
            out.append({"type":"function_call_output","call_id":message["tool_call_id"],
                "output":content if isinstance(content,str) else json.dumps(content,ensure_ascii=False)})
        elif role=="assistant" and calls:
            if message.get("content"):out.append({"role":"assistant","content":message["content"]})
            replay=get(calls[0],"openai_responses")
            if replay:
                items=replay.get("items")
                if not isinstance(items,list) or len(items)>64:raise ValueError("invalid Responses replay items")
                actual=[(item.get("call_id"),item.get("name"),json.loads(item.get("arguments") or "{}")) for item in items if item.get("type")=="function_call"]
                expected=[(c.get("id"),c["function"]["name"],json.loads(c["function"]["arguments"] or "{}")) for c in calls]
                if actual!=expected:raise ValueError("Responses replay call mismatch")
                for item in items:
                    if item.get("type")=="reasoning":
                        if set(item)-{"type","id","encrypted_content","summary"} or item.get("summary")!=[]:
                            raise ValueError("plaintext reasoning is not replay metadata")
                    elif item.get("type")!="function_call":raise ValueError("unsupported Responses replay item")
                out.extend(items)
            else:
                out.extend({"type":"function_call","call_id":c["id"],"name":c["function"]["name"],
                    "arguments":c["function"]["arguments"]} for c in calls)
        else:
            out.append({"role":role,"content":message.get("content","")})
    return out


def responses_state(output):
    items=[]
    for item in output:
        if item.get("type")=="reasoning" and item.get("encrypted_content"):
            items.append({"type":"reasoning","id":item["id"],"encrypted_content":item["encrypted_content"],"summary":[]})
        elif item.get("type")=="function_call":
            items.append({key:item[key] for key in ("type","id","call_id","name","arguments","status") if key in item})
    return bind("openai_responses",{"items":items}) if any(i["type"]=="function_call" for i in items) else None
