"""Loopback-only launcher for the real Core ML service in routing acceptance."""
import json
import os
from pathlib import Path
import socket
import sys

os.environ.update(LOG_DIR="", SHARED_SECRET="routing-e2e-ml", LLM_BASE_URL="http://127.0.0.1:9/v1", LLM_MODEL="forbidden-default")
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "ml"))
from app.main import app
import uvicorn

sock = socket.socket()
sock.bind(("127.0.0.1", 0))
sock.listen(128)
print(json.dumps({"url": "http://127.0.0.1:" + str(sock.getsockname()[1])}), flush=True)
uvicorn.Server(uvicorn.Config(app, log_level="warning", access_log=False)).run(sockets=[sock])
