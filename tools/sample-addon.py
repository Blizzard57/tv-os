#!/usr/bin/env python3
"""A minimal Stremio-compatible addon serving the Blender open movies.

Run it, then install it into TV OS:

    python3 tools/sample-addon.py &
    curl -X POST http://127.0.0.1:8484/api/addons \
         -H 'Content-Type: application/json' \
         -d '{"url": "http://127.0.0.1:7100/manifest.json"}'

It implements the two protocol resources tvosd uses — catalog and stream —
in ~80 lines of stdlib Python, so it also serves as the template for writing
your own addon: same shapes, your URLs.
"""

import json
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import unquote

PORT = 7100

BLENDER = "https://download.blender.org"
WIKI = "https://commons.wikimedia.org/wiki/Special:FilePath"

FILMS = {
    "bbb": {
        "name": "Big Buck Bunny",
        "poster": f"{WIKI}/Big_buck_bunny_poster_big.jpg?width=600",
        "streams": [
            {"name": "Blender CDN 1080p",
             "url": f"{BLENDER}/peach/bigbuckbunny_movies/big_buck_bunny_1080p_h264.mov"},
            {"name": "Blender CDN 480p",
             "url": f"{BLENDER}/peach/bigbuckbunny_movies/big_buck_bunny_480p_h264.mov"},
        ],
    },
    "sintel": {
        "name": "Sintel",
        "poster": f"{WIKI}/Sintel_poster.jpg?width=600",
        "streams": [
            {"name": "Blender CDN 720p",
             "url": f"{BLENDER}/durian/movies/Sintel.2010.720p.mkv"},
        ],
    },
    "tos": {
        "name": "Tears of Steel",
        "poster": f"{WIKI}/Tos-poster.png?width=600",
        "streams": [
            {"name": "Blender CDN 720p",
             "url": f"{BLENDER}/demo/movies/ToS/tears_of_steel_720p.mov"},
        ],
    },
}

MANIFEST = {
    "id": "org.tvos.sample",
    "version": "1.0.0",
    "name": "Blender Films",
    "description": "Open movies from the Blender Foundation",
    "resources": ["catalog", "stream"],
    "types": ["movie"],
    "catalogs": [{"type": "movie", "id": "blender", "name": "Open Movies"}],
}


class Addon(BaseHTTPRequestHandler):
    def do_GET(self):
        path = unquote(self.path)
        if path == "/manifest.json":
            return self.reply(MANIFEST)
        if path == "/catalog/movie/blender.json":
            metas = [
                {"type": "movie", "id": fid, "name": f["name"], "poster": f["poster"]}
                for fid, f in FILMS.items()
            ]
            return self.reply({"metas": metas})
        if path.startswith("/stream/movie/") and path.endswith(".json"):
            fid = path[len("/stream/movie/"):-len(".json")]
            film = FILMS.get(fid)
            return self.reply({"streams": film["streams"] if film else []})
        self.send_error(404)

    def reply(self, payload):
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):  # quiet
        pass


if __name__ == "__main__":
    print(f"sample addon on http://127.0.0.1:{PORT}/manifest.json")
    HTTPServer(("127.0.0.1", PORT), Addon).serve_forever()
