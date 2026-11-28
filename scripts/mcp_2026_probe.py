"""Local-only, standard-library MCP 2026-07-28 interoperability probe."""

import json
import os
import urllib.request


def call(method, params):
    params["_meta"] = {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "python-stdlib-mcp-probe",
            "version": "1.0",
        },
        "io.modelcontextprotocol/clientCapabilities": {},
    }
    headers = {
        "Authorization": "Bearer " + os.environ["MCP_PROBE_TOKEN"],
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": "2026-07-28",
        "Mcp-Method": method,
    }
    if method == "tools/call":
        headers["Mcp-Name"] = params["name"]
    request = urllib.request.Request(
        "http://127.0.0.1:38121/mcp",
        data=json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
        ).encode(),
        headers=headers,
    )
    with urllib.request.urlopen(request) as response:
        result = json.load(response)["result"]
    assert result["resultType"] == "complete"
    return result


assert "2026-07-28" in call("server/discover", {})["supportedVersions"]
assert any(tool["name"] == "get_guidelines" for tool in call("tools/list", {})["tools"])
assert not call("tools/call", {"name": "get_guidelines", "arguments": {}}).get("isError")
print("python-stdlib-mcp-probe 1.0: discovery, list, call passed on 2026-07-28")
