"""Tests for the asynchronous JSON-RPC loop.

These exercise the transport itself with a stubbed ``handle_request`` so the
router and orchestrator are never constructed.
"""

import asyncio
import io
import json
import os
from typing import Any

import pytest

from nerv import ipc


@pytest.fixture(autouse=True)
def _reset_ipc_globals() -> Any:
    """Each test gets a fresh loop, so the cached locks must be dropped."""
    ipc._init_lock = None
    ipc._stdout_lock = None
    yield
    ipc._init_lock = None
    ipc._stdout_lock = None


async def _run_loop(lines: list[str], monkeypatch: pytest.MonkeyPatch) -> list[dict]:
    """Feed ``lines`` into the loop over a real pipe and collect the responses."""
    read_fd, write_fd = os.pipe()
    stdin = os.fdopen(read_fd, "r")
    stdout = io.StringIO()
    monkeypatch.setattr(ipc.sys, "stdin", stdin)
    monkeypatch.setattr(ipc.sys, "stdout", stdout)

    loop_task = asyncio.create_task(ipc.run_jsonrpc_loop())
    with os.fdopen(write_fd, "w") as writer:
        for line in lines:
            writer.write(line + "\n")
        writer.flush()
    # Closing the write end signals EOF, which shuts the loop down.
    await asyncio.wait_for(loop_task, timeout=10)
    stdin.close()

    return [json.loads(line) for line in stdout.getvalue().splitlines() if line.strip()]


async def test_requests_are_handled_concurrently(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A slow request must not block a later one — that was the old bug."""
    started = asyncio.Event()
    release = asyncio.Event()

    async def fake_handle(method: str, params: dict | None) -> Any:
        if method == "slow":
            started.set()
            await release.wait()
            return {"done": "slow"}
        # This only resolves if it runs *while* "slow" is still pending. Under
        # the old serial loop nothing would release "slow" and the loop would
        # hang, failing the wait_for in _run_loop.
        await asyncio.wait_for(started.wait(), timeout=5)
        release.set()
        return {"done": "fast"}

    monkeypatch.setattr(ipc, "handle_request", fake_handle)

    responses = await _run_loop(
        [
            json.dumps({"jsonrpc": "2.0", "method": "slow", "params": {}, "id": 1}),
            json.dumps({"jsonrpc": "2.0", "method": "fast", "params": {}, "id": 2}),
        ],
        monkeypatch,
    )

    by_id = {r["id"]: r for r in responses}
    assert by_id[1]["result"] == {"done": "slow"}
    assert by_id[2]["result"] == {"done": "fast"}
    # The fast reply is written first precisely because it did not wait.
    assert responses[0]["id"] == 2


async def test_handler_error_becomes_rpc_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A raising handler produces a JSON-RPC error, not a dead connection."""

    async def fake_handle(method: str, params: dict | None) -> Any:
        raise RuntimeError("boom")

    monkeypatch.setattr(ipc, "handle_request", fake_handle)

    responses = await _run_loop(
        [json.dumps({"jsonrpc": "2.0", "method": "route", "params": {}, "id": 7})],
        monkeypatch,
    )

    assert responses[0]["id"] == 7
    assert responses[0]["error"]["code"] == -32000
    assert "boom" in responses[0]["error"]["message"]


async def test_malformed_and_incomplete_requests(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Bad input is rejected per-line without stopping the loop."""

    async def fake_handle(method: str, params: dict | None) -> Any:
        return {"ok": True}

    monkeypatch.setattr(ipc, "handle_request", fake_handle)

    responses = await _run_loop(
        [
            "{not json",
            "",
            json.dumps({"jsonrpc": "2.0", "id": 2}),
            json.dumps({"jsonrpc": "2.0", "method": "ping", "id": 3}),
        ],
        monkeypatch,
    )

    assert responses[0]["error"]["code"] == -32700  # parse error
    assert responses[1]["error"]["code"] == -32600  # missing method
    assert responses[2]["result"] == {"ok": True}


async def test_in_flight_requests_drain_on_eof(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Closing stdin still lets a running request finish and reply."""

    async def fake_handle(method: str, params: dict | None) -> Any:
        await asyncio.sleep(0.05)
        return {"late": True}

    monkeypatch.setattr(ipc, "handle_request", fake_handle)

    responses = await _run_loop(
        [json.dumps({"jsonrpc": "2.0", "method": "route", "params": {}, "id": 9})],
        monkeypatch,
    )

    assert responses == [{"jsonrpc": "2.0", "result": {"late": True}, "id": 9}]


async def test_concurrency_is_capped(monkeypatch: pytest.MonkeyPatch) -> None:
    """The semaphore bounds how many model calls can be in flight at once."""
    peak = 0
    live = 0

    async def fake_handle(method: str, params: dict | None) -> Any:
        nonlocal peak, live
        live += 1
        peak = max(peak, live)
        await asyncio.sleep(0.01)
        live -= 1
        return {"ok": True}

    monkeypatch.setattr(ipc, "handle_request", fake_handle)
    monkeypatch.setattr(ipc, "MAX_CONCURRENT_REQUESTS", 2)

    lines = [
        json.dumps({"jsonrpc": "2.0", "method": "route", "params": {}, "id": i})
        for i in range(10)
    ]
    responses = await _run_loop(lines, monkeypatch)

    assert len(responses) == 10
    assert peak <= 2


async def test_ping_does_not_build_the_orchestrator() -> None:
    """Readiness probes must stay cheap — no router, no orchestrator."""
    assert await ipc.handle_request("ping", None) == {"status": "ok"}
    assert ipc._router is None
    assert ipc._orchestrator is None
