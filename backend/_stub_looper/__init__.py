"""Stand-in for the real ``looper`` package (M2).

Mirrors the module layout and contract from ``docs/contracts-v0.md`` so that the
backend can be developed and tested before ``looper/`` exists. Activated by
``LOOPER_USE_STUB=1`` or automatically when ``import looper`` fails.
"""
