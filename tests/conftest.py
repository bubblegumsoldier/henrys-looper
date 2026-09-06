import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

EXAMPLE = ROOT / "examples" / "example-song.yaml"
FIXTURES = Path(__file__).parent / "fixtures"


def wait_for(predicate, timeout: float = 5.0, interval: float = 0.005) -> bool:
    """Poll `predicate` until true (test helper only - production code never sleeps for timing)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


@pytest.fixture
def example_yaml() -> str:
    return EXAMPLE.read_text(encoding="utf-8")


@pytest.fixture
def broken_yaml() -> str:
    return (FIXTURES / "broken-song.yaml").read_text(encoding="utf-8")
