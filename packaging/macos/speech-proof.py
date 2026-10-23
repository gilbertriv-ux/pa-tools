#!/usr/bin/env python3
"""Check a bundled model through a Launch Services app launch and its local control API.

Usage: speech-proof.py <Pa Tools.app> <speech.wav> <new-proof-directory>
The app runs without preferences or audio-device access. No model environment override is used.
"""

import json
import pathlib
import socket
import subprocess
import sys
import time


def main():
    if len(sys.argv) != 4:
        raise SystemExit(__doc__)
    app, wav, proof = (pathlib.Path(p).resolve() for p in sys.argv[1:])
    if not app.is_dir() or not wav.is_file():
        raise SystemExit("Supply an app bundle and an existing WAV fixture")
    proof.mkdir()
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    subprocess.run(
        ["open", "-n", "--env", "SOUNDCRAFT_NO_PREFS=1", "--env", "PA_SPEECH_MODELS_DIR=",
         "--stdout", str(proof / "app.stdout.log"), "--stderr", str(proof / "app.stderr.log"),
         str(app), "--args", "--no-audio", "--control", str(port), str(wav)], check=True,
    )
    deadline = time.monotonic() + 60
    while True:
        try:
            connection = socket.create_connection(("127.0.0.1", port), timeout=2)
            break
        except OSError:
            if time.monotonic() >= deadline:
                raise RuntimeError("App control server did not start; inspect app.stderr.log")
            time.sleep(0.25)
    connection.settimeout(180)
    stream = connection.makefile("rwb")
    sequence = 0

    def request(method, params):
        nonlocal sequence
        sequence += 1
        stream.write((json.dumps({"id": sequence, "method": method, "params": params}) + "\n").encode())
        stream.flush()
        response = json.loads(stream.readline())
        if not response.get("ok"):
            raise RuntimeError(response)
        return response.get("result")

    try:
        result = request("engine.execute", {"command": "track.transcribe", "params": {"diarize": False}})
        assert result["status"] == "ok", result
        assert "transcription" in result["text"].lower(), result
        request("ui.menu.invoke", {"path": "Window > Transcript"})
        request("engine.execute", {"command": "session.save_as", "params": {"path": str(proof / "Speech Demo.scraft")}})
        request("engine.execute", {"command": "session.open", "params": {"path": str(proof / "Speech Demo.scraft")}})
        parity = request("engine.parity", {})
        request("ui.screenshot", {"path": str(proof / "transcript.png")})
        assert (proof / "transcript.png").is_file()
        (proof / "proof.json").write_text(json.dumps({
            "launch": "macOS Launch Services", "model_override": "empty (ignored)",
            "transcription": result, "session_saved_and_reopened": True,
            "parity": {key: parity[key] for key in ("implemented", "total", "percent")},
        }, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        print("App proof:", proof)
    finally:
        try:
            request("engine.execute", {"command": "app.quit", "params": {}})
        finally:
            stream.close()
            connection.close()


if __name__ == "__main__":
    main()
