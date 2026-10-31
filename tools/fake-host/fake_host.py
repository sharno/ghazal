"""A stand-in for a Sunshine host's HTTP(S) API, to try the handheld's Moonlight
screens without a streaming PC: it answers `serverinfo` and `applist` only, so
host lists, "Games and apps" and the not-paired message work; pairing and
streaming need a real host.

    openssl req -x509 -newkey rsa:2048 -nodes -keyout /tmp/key.pem -out /tmp/cert.pem -days 7 -subj /CN=fake
    python3 -I tools/fake-host/fake_host.py --paired /tmp/cert.pem /tmp/key.pem

then add this computer's address as a host on the handheld. Without --paired it
reports the client as not paired, as a host that has never seen it does.
"""
import argparse
import http.server
import ssl
import threading

APPS = ["Desktop", "Steam Big Picture", "Half-Life 2: Episode One"]


def serverinfo(paired: bool, http_port: int, https_port: int) -> str:
    return f"""<?xml version="1.0" encoding="utf-8"?>
<root status_code="200"><hostname>FAKE-PC</hostname><appversion>7.1.431.-1</appversion><GfeVersion>3.23.0.74</GfeVersion>
<uniqueid>0123456789ABCDEF</uniqueid><HttpsPort>{https_port}</HttpsPort><ExternalPort>{http_port}</ExternalPort>
<mac>00:00:00:00:00:00</mac><MaxLumaPixelsHEVC>0</MaxLumaPixelsHEVC><LocalIP>127.0.0.1</LocalIP>
<ServerCodecModeSupport>259</ServerCodecModeSupport><gputype>FakeGPU</gputype><GsVersion>sunshine</GsVersion>
<SupportedDisplayMode><DisplayMode><Width>1920</Width><Height>1080</Height><RefreshRate>60</RefreshRate></DisplayMode></SupportedDisplayMode>
<PairStatus>{int(paired)}</PairStatus><currentgame>0</currentgame><state>SUNSHINE_SERVER_FREE</state></root>"""


def applist() -> str:
    apps = "".join(f"<App><IsHdrSupported>0</IsHdrSupported><AppTitle>{title}</AppTitle><ID>{n}</ID></App>" for n, title in enumerate(APPS, 1))
    return f'<?xml version="1.0" encoding="utf-8"?>\n<root status_code="200">{apps}</root>'


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("cert")
    parser.add_argument("key")
    parser.add_argument("--paired", action="store_true")
    parser.add_argument("--bind", default="0.0.0.0")
    parser.add_argument("--http-port", type=int, default=47989)
    parser.add_argument("--https-port", type=int, default=47984)
    args = parser.parse_args()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            path = self.path.split("?")[0]
            body = {"/serverinfo": lambda: serverinfo(args.paired, args.http_port, args.https_port), "/applist": applist}.get(path)
            if body is None:
                self.send_error(404)
                return
            data = body().encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/xml")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            print(f"{self.client_address[0]} {path}", flush=True)

        def log_message(self, *_args) -> None:
            pass

    secure = http.server.ThreadingHTTPServer((args.bind, args.https_port), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(args.cert, args.key)
    context.verify_mode = ssl.CERT_NONE  # the client's certificate is self-signed
    secure.socket = context.wrap_socket(secure.socket, server_side=True)
    threading.Thread(target=secure.serve_forever, daemon=True).start()
    print(f"fake host: http {args.http_port}, https {args.https_port}, {'paired' if args.paired else 'not paired'}", flush=True)
    http.server.ThreadingHTTPServer((args.bind, args.http_port), Handler).serve_forever()


if __name__ == "__main__":
    main()
