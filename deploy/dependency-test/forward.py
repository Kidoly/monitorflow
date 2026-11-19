"""The gateway of the dependency test: forwards TCP ports, so that the
machines of a private Docker network reach the central server only
through it, and the central server reaches one of them only through it.
Each argument is LISTEN_PORT:HOST:PORT. Local Docker only."""

import socket
import sys
import threading


def pipe(source, target):
    try:
        while True:
            data = source.recv(65536)
            if not data:
                break
            target.sendall(data)
    except OSError:
        pass
    finally:
        for end in (source, target):
            try:
                end.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            end.close()


def serve(listen, host, port):
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("0.0.0.0", listen))
    server.listen(64)
    while True:
        client, _ = server.accept()
        try:
            upstream = socket.create_connection((host, port), timeout=5)
            upstream.settimeout(None)
        except OSError:
            client.close()
            continue
        threading.Thread(target=pipe, args=(client, upstream), daemon=True).start()
        threading.Thread(target=pipe, args=(upstream, client), daemon=True).start()


threads = []
for spec in sys.argv[1:]:
    listen, host, port = spec.split(":")
    thread = threading.Thread(target=serve, args=(int(listen), host, int(port)), daemon=True)
    thread.start()
    threads.append(thread)
for thread in threads:
    thread.join()
