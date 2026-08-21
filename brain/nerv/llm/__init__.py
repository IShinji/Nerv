"""Unified LLM client layer.

All model I/O flows through `complete()`, which dispatches by the model
string's provider prefix. This is the single switch point for swapping the
active backend via config alone.
"""

from nerv.llm.client import CompletionResult, complete, parse_model

__all__ = ["CompletionResult", "complete", "parse_model"]
