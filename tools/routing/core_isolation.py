"""Compile a source snapshot whose parent has no Enterprise checkout."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

core = Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix="fosnie-core-isolation-") as temporary:
    root = Path(temporary).resolve()
    assert root.parent == Path(tempfile.gettempdir()).resolve() and root.name.startswith("fosnie-core-isolation-")
    snapshot = root / "fosnie-core"
    # vNext embeds public corpus/contract and predictor source identities.
    # Include these Core-owned sources, never external encoder weights or EE.
    for directory in ("backend/src", "backend/migrations", "backend/.sqlx", "crates/fosnie-protocol", "ml/app"):
        shutil.copytree(core / directory, snapshot / directory, ignore=shutil.ignore_patterns("target", ".git", "__pycache__"))
    for name in ("Cargo.toml", "Cargo.lock", "build.rs"):
        shutil.copy2(core / "backend" / name, snapshot / "backend" / name)
    assert not (root / "fosnie-enterprise").exists()
    environment = {**os.environ, "SQLX_OFFLINE": "true", "CARGO_TARGET_DIR": os.environ.get("CARGO_TARGET_DIR", str(core / "backend/target"))}
    status = subprocess.run(["cargo", "check", "--offline", "--lib"], cwd=snapshot / "backend", env=environment, check=False).returncode
    if status:
        raise SystemExit(status)
print("Standalone Core source snapshot compiled without an Enterprise sibling or licence")
