import os
import socket
import pytest
import tempfile
import shutil
from pathlib import Path
import subprocess


@pytest.fixture(scope="session")
def nostr_relay(worker_id):
    worker_index = int(worker_id[2:]) if worker_id and worker_id.startswith("gw") else 0
    base_port = 50000 + (worker_index * 100)
    port = get_free_port(base_port)

    config_path = Path(__file__).parent / "config.toml"
    if not config_path.exists():
        raise FileNotFoundError(f"config.toml not found at {config_path}")

    temp_dir = Path(tempfile.mkdtemp())

    # Copy your original config.toml into it
    original_config = Path(__file__).parent / "config.toml"
    temp_config = temp_dir / "config.toml"
    shutil.copy(original_config, temp_config)

    with temp_config.open("a") as f:
        f.write(f"port = {port}\n")

    proc = subprocess.Popen(
        ["./nostr-rs-relay", "--config", str(temp_config), "--db", str(temp_dir)],
        cwd=Path(__file__).parent,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=os.environ | {"RUST_LOG": "warn,nostr_rs_relay=debug"},
    )

    try:
        import time

        time.sleep(1.0)

        ws_url = f"ws://127.0.0.1:{port}"
        yield ws_url

    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def get_free_port(start_port=50000):
    for port in range(start_port, start_port + 100):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            try:
                s.bind(("127.0.0.1", port))
                s.listen(1)
                return port
            except OSError:
                continue
    raise RuntimeError(f"No free ports found starting from {start_port}")
