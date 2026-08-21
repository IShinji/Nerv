"""CI gate: the deterministic routing pipeline must pass the golden eval."""

from nerv.eval import GOLDEN_ROUTING, evaluate_routing_logic


def test_golden_routing_pipeline_is_fully_correct() -> None:
    report = evaluate_routing_logic()
    assert report.accuracy == 1.0, report.summary()


def test_eval_report_tracks_failures() -> None:
    from nerv.eval import RoutingCase

    bad = [
        RoutingCase(
            "x",
            '{"intent":"code_generation","agent_type":"developer","reply":""}',
            expect_agent="researcher",  # intentionally wrong expectation
        )
    ]
    report = evaluate_routing_logic(bad)
    assert report.accuracy == 0.0
    assert report.failures
    assert len(GOLDEN_ROUTING) >= 5
