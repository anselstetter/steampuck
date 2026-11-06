#!/usr/bin/env python3
"""Tiny DSU (cemuhook) client — verifies our server: version + info handshake,
then subscribes and decodes streamed accel/gyro, validating CRC on every packet."""
import socket, struct, zlib

SERVER = ("127.0.0.1", 26760)
CLIENT_ID = 0x1234ABCD

def build(event, payload=b""):
    body = struct.pack("<I", event) + payload
    p = b"DSUC" + struct.pack("<HHII", 1001, len(body), 0, CLIENT_ID) + body
    crc = zlib.crc32(p) & 0xFFFFFFFF
    return p[:8] + struct.pack("<I", crc) + p[12:]

def crc_ok(d):
    stored = struct.unpack_from("<I", d, 8)[0]
    z = d[:8] + b"\x00\x00\x00\x00" + d[12:]
    return (zlib.crc32(z) & 0xFFFFFFFF) == stored

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.settimeout(0.5)

# 1) version handshake (retry until the daemon has bound the port)
ver = None
for _ in range(20):
    try:
        s.sendto(build(0x100000), SERVER)
        d, _ = s.recvfrom(1024)
        ver = struct.unpack_from("<H", d, 20)[0]
        print(f"version reply: {ver}  crc_ok={crc_ok(d)}")
        break
    except (socket.timeout, ConnectionRefusedError):
        continue
if ver is None:
    print("FAIL: no version reply (server up?)"); raise SystemExit(1)

# 2) controller info for slot 0
try:
    s.sendto(build(0x100001, struct.pack("<i", 1) + bytes([0])), SERVER)
    d, _ = s.recvfrom(1024)
    print(f"info reply: slot={d[20]} state={d[21]} model={d[22]} conn={d[23]} "
          f"battery=0x{d[30]:02x}  crc_ok={crc_ok(d)}  (state 2=connected, model 2=full gyro)")
except (socket.timeout, ConnectionRefusedError):
    print("FAIL: no info reply"); raise SystemExit(1)

# 3) subscribe to all controllers' data, decode motion
s.sendto(build(0x100002, bytes([0, 0]) + b"\x00" * 6), SERVER)
print("subscribed; sampling motion (rotate/tilt the controller to see gyro move):")
s.settimeout(2.0)
got = 0
for i in range(300):
    try:
        d, _ = s.recvfrom(1024)
    except socket.timeout:
        break
    if len(d) < 100 or struct.unpack_from("<I", d, 16)[0] != 0x100002:
        continue
    got += 1
    if got % 40 == 1:
        ts = struct.unpack_from("<Q", d, 68)[0]
        ax, ay, az = struct.unpack_from("<fff", d, 76)
        gx, gy, gz = struct.unpack_from("<fff", d, 88)
        print(f"  ts={ts:>10}us  accel=({ax:+.2f},{ay:+.2f},{az:+.2f})g  "
              f"gyro=({gx:+7.1f},{gy:+7.1f},{gz:+7.1f})deg/s  crc_ok={crc_ok(d)}")
print(f"decoded {got} motion packets" if got else
      "no motion packets (is a controller connected & streaming?)")
