"""Puente UDP 19132 (LAN) <-> TCP 19133 (adb forward) para el servidor dentro del AVD.

El emulador está en NAT: el host recibe los datagramas de la LAN y los envía al
udpbridge del AVD por TCP (adb forward 19133). También expone el TCP 19132 por
LAN para la señalización NetherNet (/v1/join, /metrics).

Uso: python bedrock_lan_relay.py [be4|le4|raw]   (por defecto be4)
"""
import socket
import struct
import sys
import threading
import time

MODE = (sys.argv[1] if len(sys.argv) > 1 else "be4").lower()
UDP_ADDR = ("0.0.0.0", 19132)
TCP_ADDR = ("127.0.0.1", 19133)

clients = set()
clients_lock = threading.Lock()
tcp_lock = threading.Lock()
tcp_sock = None
running = True


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def connect_tcp():
    global tcp_sock
    while running:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        try:
            s.connect(TCP_ADDR)
            s.settimeout(None)
            tcp_sock = s
            log(f"TCP conectado a {TCP_ADDR} (modo {MODE})")
            return s
        except OSError as e:
            log(f"TCP connect fallo: {e}; reintentando en 1s")
            s.close()
            time.sleep(1)


def frame(data):
    if MODE == "be4":
        return struct.pack(">I", len(data)) + data
    if MODE == "le4":
        return struct.pack("<I", len(data)) + data
    return data


def udp_to_tcp(udp):
    sock = connect_tcp()
    while running:
        try:
            data, addr = udp.recvfrom(65535)
        except OSError:
            break
        with clients_lock:
            clients.add(addr)
        log(f"UDP {addr[0]}:{addr[1]} -> TCP ({len(data)} B) head={data[:8].hex()}")
        try:
            with tcp_lock:
                sock.sendall(frame(data))
        except OSError as e:
            log(f"TCP send fallo: {e}; reconectando")
            connect_tcp()
            sock = tcp_sock


def parse_frames(buf):
    out = []
    if MODE == "raw":
        if buf:
            out.append((buf, b""))
        return out, b""
    fmt = ">I" if MODE == "be4" else "<I"
    while len(buf) >= 4:
        n = struct.unpack(fmt, buf[:4])[0]
        if n > 200000:
            log(f"longitud sospechosa {n}; descartando buffer")
            return [], b""
        if len(buf) < 4 + n:
            break
        out.append((buf[4:4 + n], buf[4 + n:]))
        buf = buf[4 + n:]
    return out, buf


def tcp_to_udp(udp):
    buf = b""
    while running:
        s = tcp_sock
        if s is None:
            time.sleep(0.2)
            continue
        try:
            chunk = s.recv(65535)
        except OSError as e:
            log(f"TCP recv fallo: {e}")
            time.sleep(0.5)
            connect_tcp()
            buf = b""
            continue
        if not chunk:
            log("TCP cerrado por el extremo; reconectando")
            connect_tcp()
            buf = b""
            continue
        buf += chunk
        frames, buf = parse_frames(buf)
        for data, _ in frames:
            log(f"TCP -> UDP ({len(data)} B) head={data[:8].hex()}")
            with clients_lock:
                dests = list(clients)
            for addr in dests:
                try:
                    udp.sendto(data, addr)
                except OSError as e:
                    log(f"UDP send fallo a {addr}: {e}")


def handle_tcp_client(client):
    try:
        upstream = socket.create_connection(("127.0.0.1", 19132), timeout=5)
    except OSError as e:
        log(f"TCP 19132 proxy: no pudo conectar con adb forward: {e}")
        client.close()
        return
    log("TCP 19132 proxy: cliente conectado")

    def pump(src, dst):
        try:
            while running:
                data = src.recv(65535)
                if not data:
                    break
                dst.sendall(data)
        except OSError:
            pass
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass

    t1 = threading.Thread(target=pump, args=(client, upstream), daemon=True)
    t2 = threading.Thread(target=pump, args=(upstream, client), daemon=True)
    t1.start()
    t2.start()
    t1.join()
    t2.join()
    client.close()
    upstream.close()


def tcp_proxy():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", 19132))
    srv.listen(16)
    log("Escuchando TCP 0.0.0.0:19132 -> 127.0.0.1:19132 (adb forward)")
    while running:
        try:
            client, _ = srv.accept()
        except OSError:
            break
        threading.Thread(target=handle_tcp_client, args=(client,), daemon=True).start()


def main():
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    udp.bind(UDP_ADDR)
    log(f"Escuchando UDP {UDP_ADDR} -> TCP {TCP_ADDR}")
    threading.Thread(target=tcp_to_udp, args=(udp,), daemon=True).start()
    threading.Thread(target=tcp_proxy, daemon=True).start()
    udp_to_tcp(udp)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        running = False
