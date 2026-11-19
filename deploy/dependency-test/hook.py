"""A webhook receiver for the dependency test: appends each JSON body it
gets, one per line, to the file named by its first argument, and answers
200. Local Docker only."""

import http.server
import sys


class Hook(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        with open(sys.argv[1], "ab") as out:
            out.write(body.replace(b"\n", b" ") + b"\n")
        self.send_response(200)
        self.end_headers()

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer(("0.0.0.0", 8080), Hook).serve_forever()
