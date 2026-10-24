"""litellm is an optional extra; its absence must produce an actionable error."""

import sys

import pytest

from nerv.llm.client import complete


async def test_missing_litellm_gives_install_hint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setitem(sys.modules, "litellm", None)  # makes `import litellm` fail
    with pytest.raises(RuntimeError, match=r"nerv-brain\[litellm\]"):
        await complete([{"role": "user", "content": "hi"}], model="litellm:gpt-5")
