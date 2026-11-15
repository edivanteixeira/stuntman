"""End-to-end check of AI models (A12) over raw HTTP, the way the SDKs call: OpenAI chat
completions (tool call, then text), the Responses API (streamed), embeddings, Azure OpenAI
deployments, Anthropic messages (streamed events and tool use), Gemini generateContent, forced
failures (429, 529, cut, length, bad JSON), spend. With the official SDKs installed
(pip install openai anthropic) it also runs them against Stuntman.

    cargo build -p stuntman && python3 tests/e2e_ai.py
"""
import http.client, json

from harness import Stuntman


def post(s, path, body, headers=None, stream=False):
    c = http.client.HTTPConnection("127.0.0.1", s.PORT, timeout=30)
    h = {"Content-Type": "application/json", "Authorization": "Bearer sk-test", "User-Agent": "acme-agent/1.0"}
    h.update(headers or {})
    c.request("POST", path, json.dumps(body).encode(), h)
    r = c.getresponse()
    try:
        raw = r.read()
    except http.client.IncompleteRead as e:
        raw = e.partial + b"!CUT"
    if stream:
        return r.status, raw.decode(), dict(r.getheaders())
    try:
        return r.status, json.loads(raw), dict(r.getheaders())
    except Exception:
        return r.status, raw, dict(r.getheaders())


def events(raw):
    out = []
    for block in raw.split("\n\n"):
        name, data = None, None
        for line in block.splitlines():
            if line.startswith("event: "):
                name = line[7:]
            elif line.startswith("data: "):
                data = line[6:]
        if data and data != "[DONE]":
            out.append((name, json.loads(data)))
    return out


def sdks(s):
    try:
        import anthropic, openai
    except ImportError:
        print("  (SDKs not installed: skipped)")
        return
    oc = openai.OpenAI(base_url=f"http://127.0.0.1:{s.PORT}/openai/v1", api_key="sk-test", max_retries=0)
    r = oc.chat.completions.create(model="gpt-6.1", messages=[{"role": "user", "content": "classify this ticket"}])
    assert json.loads(r.choices[0].message.content) == {"label": "billing"} and r.usage.prompt_tokens > 0, r
    st = oc.chat.completions.create(model="gpt-6.1", messages=[{"role": "user", "content": "hello"}], stream=True, stream_options={"include_usage": True})
    text = "".join((c.choices[0].delta.content or "") for c in st if c.choices)
    assert text == "This is a simulated answer from Stuntman.", text
    rs = oc.responses.create(model="gpt-6.1", input="I need a refund", tools=[{"type": "function", "name": "lookup_order", "parameters": {"type": "object"}}])
    assert rs.output[0].type == "function_call" and rs.output[0].name == "lookup_order", rs.output
    e = oc.embeddings.create(model="text-embedding-3-small", input=["a", "b"])
    assert len(e.data) == 2 and len(e.data[0].embedding) == 1536
    ac = anthropic.Anthropic(base_url=f"http://127.0.0.1:{s.PORT}/anthropic", api_key="sk-ant-test", max_retries=0)
    m = ac.messages.create(model="claude-sonnet-5-5", max_tokens=512, messages=[{"role": "user", "content": "I want a refund"}], tools=[{"name": "lookup_order", "input_schema": {"type": "object"}}])
    assert m.stop_reason == "tool_use" and m.content[0].type == "tool_use" and m.content[0].input == {"order_id": "A-1042"}, m
    with ac.messages.stream(model="claude-sonnet-5-5", max_tokens=512, messages=[{"role": "user", "content": "hi"}]) as st:
        assert st.get_final_text() == "This is a simulated answer from Stuntman."
    s.call("POST", "/ai/force", {"provider": "anthropic", "kind": "529"})
    try:
        ac.messages.create(model="claude-sonnet-5-5", max_tokens=10, messages=[{"role": "user", "content": "hi"}])
        raise AssertionError("expected overloaded")
    except anthropic.APIStatusError as x:
        assert x.status_code == 529 and x.body["error"]["type"] == "overloaded_error", x
    print("  SDKs: ok")


def main():
    with Stuntman("stuntman-ai-") as s:
        s.call("PUT", "/ai/rules", {"name": "refund", "contains": "refund", "answer": {"text": "The refund of R$ 120,00 takes 5 days.", "tool_call": {"name": "lookup_order", "arguments": {"order_id": "A-1042"}}}})
        s.call("PUT", "/ai/rules", {"name": "classify", "contains": "classify", "json": {"label": "billing"}})

        # Chat completions: the tool call, then the text after the tool's result.
        tools = [{"type": "function", "function": {"name": "lookup_order", "parameters": {"type": "object"}}}]
        msgs = [{"role": "system", "content": "You are the support agent of Acme Seguros."}, {"role": "user", "content": "The customer wants a refund for A-1042"}]
        st, v, h = post(s, "/openai/v1/chat/completions", {"model": "gpt-6.1", "messages": msgs, "tools": tools})
        tc = v["choices"][0]["message"]["tool_calls"][0]
        assert st == 200 and v["choices"][0]["finish_reason"] == "tool_calls" and tc["function"]["name"] == "lookup_order" and json.loads(tc["function"]["arguments"]) == {"order_id": "A-1042"}, v
        msgs += [v["choices"][0]["message"], {"role": "tool", "tool_call_id": tc["id"], "content": '{"status": "paid"}'}]
        st, v, _ = post(s, "/openai/v1/chat/completions", {"model": "gpt-6.1", "messages": msgs, "tools": tools})
        assert v["choices"][0]["message"]["content"] == "The refund of R$ 120,00 takes 5 days." and v["choices"][0]["finish_reason"] == "stop", v
        # No key: 401.
        st, v, _ = post(s, "/openai/v1/chat/completions", {"model": "gpt-6.1", "messages": msgs}, {"Authorization": ""})
        assert st == 401 and v["error"]["code"] == "invalid_api_key", (st, v)

        # Responses API, streamed.
        st, raw, h = post(s, "/openai/v1/responses", {"model": "gpt-6.1", "input": "hello", "stream": True}, stream=True)
        ev = events(raw)
        assert h["content-type"] == "text/event-stream" and ev[0][0] == "response.created" and ev[-1][0] == "response.completed", [e[0] for e in ev]
        assert "".join(e[1]["delta"] for e in ev if e[0] == "response.output_text.delta") == "This is a simulated answer from Stuntman.", raw

        # Embeddings: the same text, the same vector.
        st, v, _ = post(s, "/openai/v1/embeddings", {"model": "text-embedding-3-small", "input": ["refund policy", "refund policy"], "dimensions": 64})
        assert len(v["data"]) == 2 and v["data"][0]["embedding"] == v["data"][1]["embedding"] and len(v["data"][0]["embedding"]) == 64, v

        # Azure OpenAI: a deployment, api-key header.
        st, v, _ = post(s, "/azure-openai/openai/deployments/gpt-6.1/chat/completions?api-version=2025-04-01-preview", {"messages": [{"role": "user", "content": "classify it"}]}, {"Authorization": "", "api-key": "k"})
        assert st == 200 and json.loads(v["choices"][0]["message"]["content"]) == {"label": "billing"} and v["model"] == "gpt-6.1" and "prompt_filter_results" in v, v
        s.call("POST", "/ai/force", {"provider": "azure-openai", "kind": "filter"})
        st, v, _ = post(s, "/azure-openai/openai/deployments/gpt-6.1/chat/completions?api-version=2025-04-01-preview", {"messages": [{"role": "user", "content": "x"}]}, {"api-key": "k"})
        assert st == 400 and v["error"]["code"] == "content_filter", (st, v)

        # Anthropic: streamed events, tool use.
        ah = {"x-api-key": "sk-ant", "anthropic-version": "2023-06-01", "Authorization": ""}
        st, raw, _ = post(s, "/anthropic/v1/messages", {"model": "claude-sonnet-5-5", "max_tokens": 256, "stream": True, "messages": [{"role": "user", "content": "please refund A-1042"}], "tools": [{"name": "lookup_order", "input_schema": {"type": "object"}}]}, ah, stream=True)
        ev = events(raw)
        names = [e[0] for e in ev]
        assert names[0] == "message_start" and names[-1] == "message_stop" and "content_block_start" in names, names
        args = "".join(e[1]["delta"]["partial_json"] for e in ev if e[0] == "content_block_delta" and e[1]["delta"]["type"] == "input_json_delta")
        assert json.loads(args) == {"order_id": "A-1042"} and [e for e in ev if e[0] == "message_delta"][0][1]["delta"]["stop_reason"] == "tool_use", raw
        st, v, _ = post(s, "/anthropic/v1/messages/count_tokens", {"model": "claude-sonnet-5-5", "messages": [{"role": "user", "content": "hello there"}]}, ah)
        assert v["input_tokens"] > 0, v
        s.call("POST", "/ai/force", {"provider": "anthropic", "kind": "cut"})
        st, raw, _ = post(s, "/anthropic/v1/messages", {"model": "claude-sonnet-5-5", "max_tokens": 256, "stream": True, "messages": [{"role": "user", "content": "tell me a long story"}]}, ah, stream=True)
        assert raw.endswith("!CUT") and "message_stop" not in raw, raw[-200:]

        # Gemini.
        st, v, _ = post(s, "/gemini/v1beta/models/gemini-3-flash:generateContent?key=k", {"contents": [{"role": "user", "parts": [{"text": "classify this"}]}]}, {"Authorization": ""})
        assert json.loads(v["candidates"][0]["content"]["parts"][0]["text"]) == {"label": "billing"} and v["usageMetadata"]["promptTokenCount"] > 0, v

        # Failures for any provider: 429, length, bad JSON.
        s.call("POST", "/ai/force", {"kind": "429"})
        st, v, h = post(s, "/openai/v1/chat/completions", {"model": "gpt-6.1", "messages": [{"role": "user", "content": "hi"}]})
        assert st == 429 and h.get("retry-after") == "2", (st, h)
        s.call("POST", "/ai/force", {"kind": "length"})
        st, v, _ = post(s, "/openai/v1/chat/completions", {"model": "gpt-6.1", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]})
        assert v["choices"][0]["finish_reason"] == "length", v
        s.call("POST", "/ai/force", {"kind": "bad_json"})
        st, v, _ = post(s, "/openai/v1/chat/completions", {"model": "gpt-6.1", "messages": [{"role": "user", "content": "hi"}], "tools": tools})
        try:
            json.loads(v["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"])
            raise AssertionError("arguments should be broken")
        except json.JSONDecodeError:
            pass

        calls = s.call("GET", "/ai/calls")["calls"]
        assert calls[0]["why"].startswith("forced") and any(c["why"] == "rule: refund · tool call" for c in calls), [c["why"] for c in calls]
        sp = s.call("GET", "/ai/spend")
        assert sp["cost"] > 0 and {p["provider"] for p in sp["providers"]} >= {"openai", "anthropic", "gemini", "azure-openai"}, sp
        got = s.call("GET", "/ai/calls/wait?contains=refund&within=60s")
        assert got["provider"] in ("openai", "anthropic"), got
        sdks(s)
    print("AI e2e: ok")


if __name__ == "__main__":
    main()
