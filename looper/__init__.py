"""looper - conductor library for pre-planned live looping arrangements.

Pure Python library (no FastAPI, no UI). See docs/contracts-v0.md.
"""

from .engine.base import Engine, EngineConnectionError, EngineError
from .score.compile import CompiledScore, ScoreError, compile_score

__all__ = [
    "Engine",
    "EngineError",
    "EngineConnectionError",
    "CompiledScore",
    "ScoreError",
    "compile_score",
]
