"""Versioned synthetic evaluation data; no downloads or current-world facts."""
import hashlib
import json
from pathlib import Path
import uuid

VERSION = "fosnie-routing-fixtures-v1"
ROOT = Path(__file__).resolve().parent
IDS = {name: str(uuid.uuid5(uuid.NAMESPACE_URL, VERSION + "/" + name)) for name in
       ("economy_local", "balanced_cloud", "advanced_cloud", "advanced_local", "unknown", "judge_local")}
CATEGORIES = {"weather": "test", "recipe": "train", "editing": "calibration", "proof": "test",
              "coding": "train", "long_simple": "test", "rag": "test", "tables": "calibration",
              "documents": "train", "tool_use": "calibration", "continuation": "calibration",
              "unknown": "train", "local_only": "test", "fallback": "calibration"}


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def profile(name, tier, locality, price, context=65536, auto=True):
    return {"provider_id": IDS[name], "provider_revision": 1, "revision": 1, "model": "synthetic-" + name,
            "adapter": "openai_chat", "locality": locality, "locality_trusted": True, "tier": tier,
            "auto_enabled": auto, "quality_status": "unknown" if name == "unknown" else "declared",
            "evaluation_samples": 0, "evaluation_version": None, "task_tiers": {}, "latency_ms": None,
            "capabilities": {"context_tokens": context, "max_output_tokens": 8192, "tools": "declared",
                             "vision": "unknown", "structured_output": "declared",
                             "efforts": ["high"] if tier == "advanced" else []},
            "pricing": {"input_microusd_per_million": price, "output_microusd_per_million": price,
                        "source": "synthetic fixture tariff; not a provider price", "checked_at": None}}


def policies():
    rules = [{"id": str(uuid.uuid5(uuid.NAMESPACE_URL, VERSION + "/simple-rule")), "priority": 20,
              "conditions": {"tasks": ["general", "editing", "extraction", "current_fact"], "max_input_tokens": 7000},
              "action": {"kind": "select", "provider_id": IDS["economy_local"]}}]
    return {strategy: {"id": str(uuid.uuid5(uuid.NAMESPACE_URL, VERSION + "/policy/" + strategy)), "revision": 1,
                       "rollout": "active", "strategy": strategy, "objective": "economy", "rules": rules if strategy != "classifier" else [],
                       "default_provider_id": IDS["advanced_cloud"], "classifier_provider_id": None if strategy == "rules" else IDS["judge_local"],
                       "constraints": {"max_calls": 5, "max_total_tokens": 100000}, "utility_assignments": {},
                       "technical_fallback": True} for strategy in ("rules", "classifier", "hybrid")}


def problem(category, n, language):
    ru = language == "ru"
    source = f"frozen:{category}:{n}"
    context, tools, helpers = [], [], []
    task, tier, validator = "general", "economy", "json_exact"
    if category == "weather":
        city = ["Glasgow", "London", "Edinburgh", "Tokyo", "Dublin"][n]
        answer = {"city": city, "temperature_c": [7, 12, -2, 18, 4][n], "source_id": source}
        tools = [{"name": "frozen_weather", "source_id": source, "as_of": "2024-01-10T12:00:00Z", "result": answer}]
        prompt = f"Верни погоду для {city} из замороженного результата." if ru else f"Return weather for {city} from the frozen result."
        task = "current_fact"
    elif category == "recipe":
        factor = n + 2
        context = [{"source_id": source, "flour_g_per_serving": 80 + n, "meat_g_per_serving": 100 + n, "servings": factor}]
        answer = {"flour_g": (80 + n) * factor, "meat_g": (100 + n) * factor, "servings": factor}
        prompt = "Пересчитай обычный рецепт мясных пирожков по числу порций." if ru else "Scale an ordinary meat-pie recipe to the requested servings."
    elif category == "editing":
        text = (["Привет, мир!", "Мы проверяем текст.", "Встреча завтра.", "Спасибо за помощь.", "Это простой пример."] if ru else
                ["Hello, world!", "We review the text.", "The meeting is tomorrow.", "Thanks for your help.", "This is a simple example."])[n]
        context = [{"text": "  " + text + "   "}]
        answer = {"text": text}
        prompt = "Удали только пробелы по краям." if ru else "Remove only leading and trailing whitespace."
        task = "editing"
    elif category == "proof":
        p = [101, 103, 107, 109, 113][n]
        a = next(v for v in range(2, p) if pow(v, (p - 1) // 2, p) == p - 1)
        context = [{"p": p, "a": a}]
        answer = {"p": p, "a": a, "euler": p - 1}
        prompt = "Докажи квадратичную невычетность и верни сертификат Эйлера." if ru else "Prove quadratic nonresiduosity and return its Euler certificate."
        task, tier, validator = "reasoning", "advanced", "euler_certificate"
    elif category == "coding":
        graphs = [[(0, 1), (1, 2)], [(0, 2), (1, 2)], [(2, 0), (0, 1)], [(1, 0), (1, 2)], [(2, 1), (1, 0)]]
        edges = [list(edge) for edge in graphs[n]]
        order = next(order for order in ([0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0])
                     if all(order.index(a) < order.index(b) for a, b in edges))
        context = [{"nodes": [0, 1, 2], "edges": edges}]
        answer = {"order": order}
        prompt = "Проверь DAG и верни корректный топологический порядок." if ru else "Validate the DAG and return a correct topological order."
        task, tier, validator = "coding", "advanced", "topological_order"
    elif category == "long_simple":
        rows = [(i * (n + 1)) % 17 for i in range(1800)]
        context = [{"source_id": source, "rows": "\n".join(f"record-{i:05d},{v}" for i, v in enumerate(rows))}]
        answer = {"count": len(rows), "total": sum(rows)}
        prompt = "Посчитай число строк и сумму значений; задача простая, объём большой." if ru else "Count rows and sum their values; simple processing of a long input."
        task = "extraction"
    elif category in ("rag", "documents", "local_only"):
        context = [{"source_id": source, "limit": 400 + 37 * n, "owner": f"team-{n}", "synthetic_private": category == "local_only"}]
        answer = {"limit": 400 + 37 * n, "source_id": source}
        prompt = "Извлеки лимит из предоставленного документа и укажи источник." if ru else "Extract the limit from the supplied document and cite its source."
        task = "extraction"
        if category == "documents":
            helpers = ["utility_summary"]
    elif category == "tables":
        rows = [{"units": i + n + 1, "price": 10 + i} for i in range(5)]
        context = [{"rows": rows}]
        answer = {"total": sum(r["units"] * r["price"] for r in rows)}
        prompt = "Посчитай итог таблицы: количество × цена." if ru else "Calculate the table total: units times price."
        task = "extraction"
    elif category == "tool_use":
        tools = [{"name": "frozen_lookup", "query_id": f"query-{n}", "result": 50 + n}]
        answer = {"query_id": f"query-{n}", "value": 50 + n, "external_actions": 0}
        prompt = "Используй только предоставленный результат; не выполняй внешние действия." if ru else "Use only the supplied result; perform no external action."
    elif category == "continuation":
        context = [{"previous_task": "coding", "known_minimum_tier": "advanced", "required_effort": "high", "invariant": n + 10}]
        answer = {"invariant": n + 10}
        prompt = "Продолжай" if ru else "Continue"
        task, tier = "coding", "advanced"
    elif category == "unknown":
        answer = None
        prompt = "Недостаточно данных о неизвестной модели." if ru else "Insufficient evidence about an unknown model."
        task, validator = "unknown", "unknown"
    else:
        answer = {"value": n + 20}
        prompt = "Верни значение из контекста." if ru else "Return the context value."
        context = [answer]
    return prompt, context, tools, answer, task, tier, validator, helpers


def build():
    catalog = [profile("economy_local", "economy", "local", 50000, 8192),
               profile("balanced_cloud", "balanced", "cloud", 500000),
               profile("advanced_cloud", "advanced", "cloud", 3000000),
               profile("advanced_local", "advanced", "local", 1500000),
               profile("unknown", "economy", "local", None, 8192),
               profile("judge_local", "economy", "local", 50000, 8192, False)]
    cases = []
    for category, split in CATEGORIES.items():
        for n in range(5):
            for language in ("en", "ru"):
                prompt, context, tools, answer, task, tier, validator, helpers = problem(category, n, language)
                messages = [{"role": "system", "content": "Synthetic frozen task. Return one JSON object only. Use supplied data only; do not browse, invoke tools or take actions."},
                            {"role": "user", "content": prompt + "\n" + encoded({"context": context, "frozen_tool_results": tools})}]
                if answer is not None:
                    messages[0]["content"] += " Output exactly these keys: " + ", ".join(sorted(answer)) + "."
                input_tokens = len(encoded({"messages": messages, "tools": None}).encode()) + 1024
                observed = {"task": "unknown" if category in ("proof", "coding", "unknown") else task,
                            "minimum_tier": "advanced" if category == "continuation" else "economy",
                            "input_tokens": input_tokens, "output_tokens": 256, "language": language,
                            "tools": False, "vision": False, "structured_output": True,
                            "effort": "high" if category == "continuation" else None, "uncertain": False}
                judged = {"task": task, "minimum_tier": tier, "fresh_information": False, "tools": False,
                          "vision": False, "structured_output": True, "confidence": 0.95, "uncertain": task == "unknown"}
                overrides, constraints, profile_overrides = {}, {}, {}
                expect_denied = False
                if category == "local_only":
                    constraints["local_only"] = True
                if category == "unknown":
                    overrides = {"default_provider_id": IDS["unknown"], "rules": [{"id": str(uuid.uuid5(uuid.NAMESPACE_URL, VERSION + "/unknown-pool")),
                        "priority": 100, "conditions": {}, "action": {"kind": "restrict_pool", "allowed_providers": [] if n == 3 else [IDS["unknown"]]}}]}
                    constraints["allowed_providers"] = [IDS["unknown"], IDS["judge_local"]]
                    expect_denied = n != 1
                    if n in (1, 2):
                        profile_overrides[IDS["unknown"]] = {"quality_status": "declared"}
                        judged["task"], judged["uncertain"] = "general", False
                    if n == 2:
                        constraints["max_cost_microusd"] = 10000
                    if n == 4:
                        judged["task"] = "invalid_enum"
                for objective in ("economy", "balanced", "quality"):
                    case_id = f"{category}-{n}-{language}-{objective}"
                    cases.append({"schema_version": 1, "id": case_id, "family_id": f"{category}/{n}", "split_group": category,
                                  "split": split, "category": category, "language": language, "difficulty": tier,
                                  "data_origin": "synthetic", "messages": messages, "context": context, "tools": tools,
                                  "requirements": observed, "objective": objective, "policy_overrides": overrides,
                                  "constraints": constraints, "profile_overrides": profile_overrides,
                                  "classifier_reply": encoded(judged), "classifier_failure": category == "unknown" and n == 0,
                                  "primary_failure": ["rate_limited", "connection", "mid_stream", "cancelled", "unauthorized"][n] if category == "fallback" else None,
                                  "helpers": helpers, "validator": {"kind": validator, "expected": answer},
                                  "expected": {"minimum_tier": tier, "deny_auto": expect_denied},
                                  "mock_answers": {p["provider_id"]: (None if answer is None else encoded({"incorrect": True} if tier == "advanced" and p["tier"] != "advanced" else answer)) for p in catalog},
                                  "smoke": n == 0 and objective == "economy"})
    return {"schema_version": 1, "version": VERSION, "seed": 0, "data_origin": "synthetic", "model_quality": "not_run",
            "split_policy": "entire semantic template categories stay in one split; language/objective/parameter variants never cross",
            "ids": IDS, "profiles": catalog, "policies": policies(), "case_count": len(cases)}, cases


def write():
    manifest, cases = build()
    payload = "".join(encoded(c) + "\n" for c in cases).encode()
    manifest["cases_sha256"] = hashlib.sha256(payload).hexdigest()
    directory = ROOT / "fixtures"
    directory.mkdir(exist_ok=True)
    (directory / "cases.jsonl").write_bytes(payload)
    (directory / "manifest.json").write_text(json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {len(cases)} synthetic cases; no network or inference")


if __name__ == "__main__":
    write()
