"""Calibrate the independent grader against real work and a known bad neighbor."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def check(grader: Path, workspace: Path, base: Path, case: Path, accepted: bool) -> dict:
    result = subprocess.run([sys.executable, str(grader), str(workspace), str(base), str(case)],
                            capture_output=True, text=True, check=False)
    receipt = json.loads(result.stdout)
    assert (result.returncode == 0) == accepted, receipt
    assert receipt["kind"] == ("passed" if accepted else "failed"), receipt
    return receipt


def main() -> None:
    base, books, devices, books_case, devices_case = map(Path, sys.argv[1:])
    grader = Path(__file__).with_name("grade.py")
    receipts = {
        "books": check(grader, books, base, books_case, True),
        "devices": check(grader, devices, base, devices_case, True),
        "no_execution": check(grader, base, base, books_case, False),
    }
    with tempfile.TemporaryDirectory(prefix="learned-grader-control-") as scratch:
        mutant = Path(scratch).joinpath("workspace")
        shutil.copytree(books, mutant, ignore=shutil.ignore_patterns(
            ".burin", ".harn", ".harn-runs", "__pycache__"))
        source = mutant.joinpath("app/resources/books.py")
        original = source.read_text()
        guard = 'type(payload["pages"]) is not int'
        assert original.count(guard) == 1, "known negative control was not constructed"
        source.write_text(original.replace(guard, 'not isinstance(payload["pages"], int)'))
        receipts["boolean_as_integer"] = check(grader, mutant, base, books_case, False)
        assert "True" in receipts["boolean_as_integer"]["error"], receipts
    print(json.dumps({"kind": "calibrated", "positive_count": 2, "negative_count": 2,
                      "receipts": receipts}, sort_keys=True))


if __name__ == "__main__":
    main()
