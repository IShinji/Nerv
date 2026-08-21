"""Minimal eval harness for routing — guards against regressions.

Two modes:
  * Offline (deterministic): feed canned classifier JSON through the Router's
    parse + sanitize pipeline and check the normalized routing. Run in CI via
    tests/test_eval_routing.py.
  * Live: `python -m nerv.eval` classifies each case with the real configured
    router model and reports intent/agent accuracy. Useful for spot-checking
    model quality without wiring up a full benchmark.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from pathlib import Path

from nerv.router.router import Router


@dataclass
class RoutingCase:
    """One routing expectation."""

    message: str
    # Canned classifier output used by the offline (deterministic) check.
    raw_model_json: str
    expect_agent: str
    expect_reply_suppressed: bool = False


@dataclass
class EvalReport:
    """Aggregate result of a routing eval run."""

    total: int = 0
    passed: int = 0
    failures: list[str] = field(default_factory=list)

    @property
    def accuracy(self) -> float:
        return self.passed / self.total if self.total else 0.0

    def summary(self) -> str:
        lines = [f"routing eval: {self.passed}/{self.total} ({self.accuracy:.0%})"]
        lines.extend(f"  FAIL: {f}" for f in self.failures)
        return "\n".join(lines)


GOLDEN_ROUTING: list[RoutingCase] = [
    RoutingCase(
        "help me debug this python function",
        '{"intent":"code_generation","complexity":"medium","model_tier":1,"agent_type":"developer","reply":""}',
        expect_agent="coder",
    ),
    RoutingCase(
        "compare postgres and mysql for analytics",
        '{"intent":"research","complexity":"high","model_tier":2,"agent_type":"analyst","reply":""}',
        expect_agent="researcher",
    ),
    RoutingCase(
        "translate this paragraph to french",
        '{"intent":"writing","complexity":"low","model_tier":1,"agent_type":"translator","reply":""}',
        expect_agent="writer",
    ),
    RoutingCase(
        "restart nginx on the server",
        '{"intent":"sysadmin","complexity":"medium","model_tier":1,"agent_type":"devops","reply":""}',
        expect_agent="sysadmin",
    ),
    RoutingCase(
        "你好",
        '{"intent":"general","complexity":"low","model_tier":0,"agent_type":"personal AI assistant","reply":"你好，有什么可以帮你？"}',  # noqa: E501
        expect_agent="general",
    ),
    RoutingCase(
        "现在几点",
        '{"intent":"general","complexity":"low","model_tier":0,"agent_type":"assistant","reply":"现在几点？"}',
        expect_agent="general",
        expect_reply_suppressed=True,
    ),
]


def evaluate_routing_logic(cases: list[RoutingCase] | None = None) -> EvalReport:
    """Offline check of the deterministic parse + sanitize pipeline."""
    cases = cases or GOLDEN_ROUTING
    router = Router()
    report = EvalReport(total=len(cases))

    for case in cases:
        parsed = router._parse_response(case.raw_model_json)
        result = router._sanitize_route_result(case.message, parsed)

        problems = []
        if result.agent_type != case.expect_agent:
            problems.append(f"agent {result.agent_type!r}!={case.expect_agent!r}")
        if case.expect_reply_suppressed and result.reply:
            problems.append("reply not suppressed")

        if problems:
            report.failures.append(f"{case.message!r}: {', '.join(problems)}")
        else:
            report.passed += 1

    return report


async def evaluate_routing_live(cases: list[RoutingCase] | None = None) -> EvalReport:
    """Classify each case with the real configured router model."""
    cases = cases or GOLDEN_ROUTING
    router = Router(project_root=Path.cwd())
    report = EvalReport(total=len(cases))

    for case in cases:
        result = await router.classify(case.message)
        if result.agent_type == case.expect_agent:
            report.passed += 1
        else:
            report.failures.append(
                f"{case.message!r}: got {result.agent_type!r}, "
                f"want {case.expect_agent!r}"
            )

    return report


def main() -> None:
    """Run the live routing eval against the configured router model."""
    report = asyncio.run(evaluate_routing_live())
    print(report.summary())


if __name__ == "__main__":
    main()
