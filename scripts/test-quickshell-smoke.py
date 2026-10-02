#!/usr/bin/env python3
"""SPEC-OBS-004: headless native QML, fragmented RPC, restart, and timeout smoke."""

import json
import os
from pathlib import Path
import queue
import shutil
import socket
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parent.parent


def main():
    errors = queue.Queue()
    logs = queue.Queue()
    stop = threading.Event()
    methods = []
    profile_id = "00000000-0000-0000-0000-000000000001"
    base = dict(capture_active=True, active_profile=profile_id, selected_source="Test game",
                input_fps=60, analysis_fps=10, last_analysis_age_ms=10,
                detector_errors=0, replaced_frames=0)
    states = [dict(base, capture_active=False, active_profile=None),
              dict(base, capture_active=False), dict(base, active_profile=None),
              dict(base, last_analysis_age_ms=None), base,
              dict(base, last_analysis_age_ms=30_000), dict(base, capture_error="source closed")]

    with tempfile.TemporaryDirectory(prefix="yash-quickshell-") as directory:
        path = str(Path(directory) / "control.sock")
        server = socket.socket(socket.AF_UNIX)
        server.bind(path)
        server.listen()
        server.settimeout(0.2)

        def serve():
            try:
                session = 0
                state_index = 0
                while not stop.is_set():
                    try:
                        connection, _ = server.accept()
                    except socket.timeout:
                        continue
                    session += 1
                    with connection:
                        connection.settimeout(8)
                        stream = connection.makefile("rb")
                        negotiated = False
                        while not stop.is_set():
                            line = stream.readline()
                            if not line:
                                break
                            request = json.loads(line)
                            method = request["method"]
                            methods.append(method)
                            assert negotiated or method == "system.handshake", "Missing handshake"
                            if session == 3:
                                connection.sendall(b"malformed\n")
                                break
                            if method == "system.handshake":
                                assert request["params"]["protocol"] == 1
                                negotiated = True
                                result = dict(protocol=1, daemon_version="test", daemon_instance="test")
                            elif method == "profile.get":
                                assert request["params"]["profile_id"] == profile_id
                                result = dict(id=profile_id, name="Test profile")
                            elif method == "system.status":
                                if session == 2:
                                    stop.wait(6)  # Force the client's response watchdog.
                                    break
                                if session == 1 and state_index == len(states):
                                    break  # Simulate a daemon restart.
                                result = states[state_index] if session == 1 else dict(base, selected_source="Recovered")
                                state_index += session == 1
                            else:
                                raise AssertionError("Unexpected method " + method)
                            data = (json.dumps(dict(jsonrpc="2.0", id=request["id"], result=result)) + "\n").encode()
                            connection.sendall(data[:9])
                            connection.sendall(data[9:])
                        stream.close()
            except Exception as error:
                if not stop.is_set():
                    errors.put(error)

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        environment = dict(os.environ, QT_QPA_PLATFORM="offscreen", YASH_QUICKSHELL_TEST_SOCKET=path)
        config = Path(directory) / "config"
        config.mkdir()
        for filename in ("StatusService.qml", "Status.js", "yash-events.svg"):
            shutil.copyfile(ROOT / "integrations/quickshell" / filename, config / filename)
        shutil.copyfile(ROOT / "integrations/quickshell/tests/smoke.qml", config / "shell.qml")
        process = subprocess.Popen(
            ["quickshell", "--no-color", "-p", str(config / "shell.qml")],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

        def collect():
            for line in process.stdout:
                logs.put(line.rstrip())

        threading.Thread(target=collect, daemon=True).start()
        seen = set()
        timed_out = False
        malformed = False
        recovered = False
        icon_ready = False
        transcript = []
        try:
            deadline = time.monotonic() + 50
            while time.monotonic() < deadline and not recovered:
                if not errors.empty():
                    raise errors.get()
                if process.poll() is not None:
                    raise AssertionError("Quickshell exited early\n" + "\n".join(transcript))
                try:
                    line = logs.get(timeout=0.2)
                except queue.Empty:
                    continue
                transcript.append(line)
                if "YASH_ICON_READY" in line:
                    icon_ready = True
                if "YASH_VIEW " not in line:
                    continue
                view = json.loads(line.split("YASH_VIEW ", 1)[1])
                if view["label"] in ("Stopped", "Idle", "Offline"):
                    assert view["profile"] == "None", view
                    assert "Profile:" not in view["tooltip"], view
                seen.add(view["label"])
                timed_out |= "timed out" in view["tooltip"]
                malformed |= "Malformed daemon reply" in view["tooltip"]
                recovered = view["label"] == "Processing" and "Recovered" in view["tooltip"]
            expected = {"Offline", "Idle", "Stopped", "Capturing", "Waiting", "Processing", "Stalled", "Error"}
            assert expected <= seen, f"Missing states {expected - seen}\n" + "\n".join(transcript)
            assert timed_out, "Watchdog did not report timeout"
            assert malformed, "Malformed reply was not rejected"
            assert recovered, "Did not reconnect to the recovered daemon"
            assert icon_ready, "SVG icon failed to load\n" + "\n".join(transcript)
            assert methods.count("system.handshake") == 4, methods
            print("Native Quickshell smoke passed: SVG icon, hidden stopped profile, eight states, fragmented framing, restart, timeout, malformed reply, recovery.")
        finally:
            stop.set()
            process.terminate()
            process.wait(timeout=5)
            thread.join(timeout=2)
            server.close()


if __name__ == "__main__":
    main()
