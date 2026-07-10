import sys

from longmemeval.run import parse_args


def test_backend_defaults_to_ram_a(monkeypatch):
    monkeypatch.setattr(sys, "argv", ["run.py"])

    args = parse_args()

    assert args.backend == "RAM-A"


def test_backend_accepts_ram_a(monkeypatch):
    monkeypatch.setattr(sys, "argv", ["run.py", "--backend", "RAM-A"])

    args = parse_args()

    assert args.backend == "RAM-A"
