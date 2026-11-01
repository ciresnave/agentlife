# SPDX-License-Identifier: MIT OR Apache-2.0
"""Scratch-only MCP reconnect probe. Never touches the live synapsed.

usage: python -I reconnect_probe.py <gap_seconds|control> <tag>
Env SYNAPSE_HOME / SYNAPSE_ADDR must point at the scratch home and port.
"""
import json, os, subprocess, sys, threading, time

H = os.environ["SYNAPSE_HOME"]
ADDR = os.environ["SYNAPSE_ADDR"]
assert "reconnect-scratch" in H and ADDR.endswith(":7941"), "refusing: not the scratch home/port"
gap = sys.argv[1]
tag = sys.argv[2]
t0 = time.time()
events = []  # (t, kind, detail)
lock = threading.Lock()


def log(kind, detail=""):
    with lock:
        events.append((round(time.time() - t0, 2), kind, detail))
        print(f"{events[-1][0]:8.2f} {kind} {detail}", flush=True)


def start_daemon():
    f = open(os.path.join(H, f"synapsed-{tag}-{int(time.time())}.log"), "wb")
    p = subprocess.Popen(["synapsed"], stderr=f, stdout=f, env=os.environ)
    log("daemon-start", f"pid={p.pid}")
    return p


def port_open():
    import socket
    s = socket.socket()
    s.settimeout(0.5)
    try:
        s.connect(("127.0.0.1", 7941))
        return True
    except OSError:
        return False
    finally:
        s.close()


def wait_port(timeout=20):
    t = time.time()
    while time.time() - t < timeout:
        if port_open():
            return round(time.time() - t, 2)
        time.sleep(0.1)
    return None


def pid_listening():
    out = subprocess.run(["netstat", "-ano", "-p", "tcp"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        if ":7941" in line and "LISTENING" in line:
            return int(line.split()[-1])
    return None


daemon = start_daemon()
log("port-up-after", str(wait_port()))
daemon_pid = daemon.pid
log("listening-pid", str(daemon_pid))

ad = subprocess.Popen(
    ["synapse-claude-channel", "--role", "probe"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=os.environ,
)
log("adapter-start", f"pid={ad.pid}")
seen = {}


def reader(stream, name):
    for raw in iter(stream.readline, b""):
        line = raw.decode("utf-8", "replace").strip()
        if not line:
            continue
        if name == "out":
            try:
                m = json.loads(line)
            except Exception:
                log("out-nonjson", line[:160])
                continue
            meth = m.get("method")
            if meth and "channel" in meth:
                body = json.dumps(m.get("params"))
                mid = None
                for k in ("message_id",):
                    if k in body:
                        i = body.index(k)
                        mid = body[i:i + 80]
                log("PUSH", f"{meth} {mid}")
                seen.setdefault(tag + "?" + str(mid), time.time())
            elif "id" in m:
                log("rpc-reply", f"id={m['id']} " + ("error" if "error" in m else "ok"))
            else:
                log("notif", str(meth))
        else:
            log("adapter-stderr", line[:200])


threading.Thread(target=reader, args=(ad.stdout, "out"), daemon=True).start()
threading.Thread(target=reader, args=(ad.stderr, "err"), daemon=True).start()


def rpc(obj):
    ad.stdin.write((json.dumps(obj) + "\n").encode())
    ad.stdin.flush()


rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2025-11-25", "capabilities": {"experimental": {"claude/channel": {}}},
    "clientInfo": {"name": "reconnect-probe", "version": "0"}}})
time.sleep(1)
rpc({"jsonrpc": "2.0", "method": "notifications/initialized"})
rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
time.sleep(3)

sent = {}


def send(i):
    mid = f"{tag}-{i:03d}"
    r = subprocess.run(["synapse", "send", "--role", "sender", "--to", "probe", "--id", mid, f"probe {mid}"],
                       capture_output=True, text=True, env=os.environ)
    sent[mid] = time.time()
    log("sent", f"{mid} rc={r.returncode} {""}")


n = 0
for _ in range(5):  # warm-up, daemon up
    send(n); n += 1
    time.sleep(2)

restart_t = None
if gap != "control":
    g = float(gap)
    log("KILL-daemon", f"pid={daemon_pid}")
    subprocess.run(["taskkill", "/PID", str(daemon_pid), "/F"], capture_output=True)
    kill_t = time.time()
    time.sleep(1)
    log("port-open-after-kill", str(port_open()))
    # observe the gap WITHOUT sending (synapse send would auto-start a daemon)
    while time.time() - kill_t < g:
        time.sleep(1)
        if port_open():
            log("port-came-back-by-itself", f"after {round(time.time()-kill_t,1)}s pid={pid_listening()}")
            break
    log("RESTART-daemon")
    daemon = start_daemon()
    restart_t = time.time()
    log("port-up-after", str(wait_port()))
else:
    time.sleep(10)
    restart_t = time.time()
    log("CONTROL-no-restart")

first_after = None
for _ in range(20):  # 40 s of post-restart sends
    mid = f"{tag}-{n:03d}"
    send(n); n += 1
    time.sleep(2)

log("adapter-alive", str(ad.poll() is None))
log("final-listening-pid", str(pid_listening()))
# summarize
pushes = [e for e in events if e[1] == "PUSH"]
post = [e for e in pushes if restart_t and e[0] >= restart_t - t0]
log("SUMMARY", f"pushes_total={len(pushes)} pushes_after_restart={len(post)} first_push_after_restart_s="
    f"{(post[0][0] - (restart_t - t0)) if post else None}")
json.dump(events, open(os.path.join(H, f"events-{tag}.json"), "w"))
ad.kill()
if daemon.poll() is None:
    daemon.kill()
log("cleanup", "adapter and scratch daemon killed")
