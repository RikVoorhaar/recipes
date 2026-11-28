"""Local-only probe using the official MCP Python SDK; see README for setup."""

import asyncio
import importlib.metadata
import os
import sys

import httpx2
from mcp import Client
from mcp.client.streamable_http import streamable_http_client


async def main():
    assert importlib.metadata.version("mcp") == "2.3.0"
    async with httpx2.AsyncClient(
        headers={"Authorization": "Bearer " + os.environ["MCP_PROBE_TOKEN"]},
        trust_env=False,
    ) as http:
        transport = streamable_http_client("http://127.0.0.1:38122/mcp", http_client=http)
        async with Client(transport, mode="2026-07-28") as client:
            assert client.protocol_version == "2026-07-28"
            discovery = await client.session.send_discover("2026-07-28")
            assert "2026-07-28" in discovery["supportedVersions"]
            assert discovery["ttlMs"] == 0 and discovery["cacheScope"] == "private"
            listing = await client.list_tools()
            assert listing.ttl_ms == 0 and listing.cache_scope == "private"
            assert any(tool.name == "get_guidelines" for tool in listing.tools)
            result = await client.call_tool("get_guidelines", {})
            assert not result.is_error and result.content
            resources = await client.list_resources()
            assert resources.ttl_ms == 0 and resources.cache_scope == "private"
            resource = await client.read_resource("recipe://guidelines")
            assert resource.ttl_ms == 0 and resource.cache_scope == "private"
            assert resource.contents
            print(
                f"mcp==2.3.0 Python {sys.version.split()[0]} protocol "
                f"{client.protocol_version}: discovery, tools list/call, resources list/read passed"
            )


asyncio.run(main())
