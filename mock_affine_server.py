"""
Minimal mock AFFiNE server for testing Zed collab's AFFiNE sign-in flow.

Endpoints:
  GET  /sign-in?redirect_uri=<url>      — Sign-in page with a button
  POST /api/auth/open-app/exchange       — Exchange code for user info
"""

import http.server
import json
import secrets
import urllib.parse

# In-memory store for one-time codes
pending_codes: dict[str, dict] = {}

MOCK_USER = {
    "id": "affine-user-001",
    "email": "testuser@example.com",
    "name": "Test User",
    "avatar_url": None,
}


class MockAffineHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)

        if parsed.path == "/sign-in":
            params = urllib.parse.parse_qs(parsed.query)
            redirect_uri = params.get("redirect_uri", [None])[0]
            if not redirect_uri:
                self.send_error(400, "Missing redirect_uri")
                return

            # Generate a one-time code
            code = secrets.token_urlsafe(32)
            pending_codes[code] = MOCK_USER.copy()

            # Build the redirect URL with the code appended
            separator = "&" if "?" in redirect_uri else "?"
            final_redirect = f"{redirect_uri}{separator}code={urllib.parse.quote(code)}"

            # Serve a simple sign-in page
            html = f"""<!DOCTYPE html>
<html>
<head><title>Mock AFFiNE Sign-In</title>
<style>
    body {{
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
        display: flex; justify-content: center; align-items: center;
        min-height: 100vh; margin: 0;
        background: #1e1e2e; color: #cdd6f4;
    }}
    .container {{ text-align: center; }}
    .btn {{
        display: inline-block; padding: 14px 32px; margin: 12px;
        border-radius: 8px; text-decoration: none; font-size: 16px;
        font-weight: 600; background: #89b4fa; color: #1e1e2e;
    }}
    .btn:hover {{ background: #74c7ec; }}
    .info {{ color: #a6adc8; font-size: 14px; margin-top: 20px; }}
</style>
</head>
<body>
    <div class="container">
        <h1>Mock AFFiNE Sign-In</h1>
        <p>Signing in as: <strong>{MOCK_USER['name']}</strong> ({MOCK_USER['email']})</p>
        <a href="{final_redirect}" class="btn">Sign In</a>
        <p class="info">This is a mock server for testing the Zed collab AFFiNE auth flow.</p>
    </div>
</body>
</html>"""
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(html.encode())
        else:
            self.send_error(404)

    def do_POST(self):
        parsed = urllib.parse.urlparse(self.path)

        if parsed.path == "/api/auth/open-app/exchange":
            content_length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(content_length)
            try:
                data = json.loads(body)
            except json.JSONDecodeError:
                self.send_error(400, "Invalid JSON")
                return

            code = data.get("code")
            if not code or code not in pending_codes:
                self.send_response(401)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(json.dumps({"error": "Invalid or expired code"}).encode())
                return

            user = pending_codes.pop(code)
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(user).encode())
        else:
            self.send_error(404)

    def log_message(self, format, *args):
        print(f"[mock-affine] {format % args}")


if __name__ == "__main__":
    port = 3010
    server = http.server.HTTPServer(("0.0.0.0", port), MockAffineHandler)
    print(f"Mock AFFiNE server running on http://0.0.0.0:{port}")
    server.serve_forever()
