#!/usr/bin/env python3
"""Mock AudioShare Maker API (koppelspecificatie v0.5) for testing StudioRecorder.

In-memory, single process. Extra test-only routes under /test/*:
  POST /test/claim-code   {"user": "Jan de Vries"}  -> {"code": ..., "file": {...}}
  POST /test/revoke       {"user": "Jan de Vries"}  -> revoke all tokens of user (next call 401)
  POST /test/fail-next    {"status": 503, "count": 2} -> next N /api/v1/me/* calls fail
  POST /test/publish      {"program_id": 4, "date": "...", "user": "...", "downloaded": true}
  GET  /test/state
"""
import hashlib, json, secrets, sys, threading, datetime, re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Lock()
USERS = {
    "Jan de Vries": {"id": 12, "username": "jan", "display_name": "Jan de Vries", "programs": [4, 5]},
    "Pietje Puk": {"id": 13, "username": "pietje", "display_name": "Pietje Puk", "programs": [4]},
}
QUALITY = {
    "formats": ["mp3", "flac", "ogg"], "min_bitrate_kbps": 128, "samplerates": [44100, 48000],
    "min_duration_sec": 5, "max_duration_sec": 5400, "max_file_mb": 200,
    "lufs_min": -23, "lufs_max": -14, "true_peak_max": -1,
}
PROGRAMS = {
    4: {"id": 4, "slug": "DeBlik1", "name": "De Blik uur 1", "description": "Uur 1", "station_neutral": True, "local_news": False, "quality": QUALITY},
    5: {"id": 5, "slug": "DeBlik2", "name": "De Blik uur 2", "description": "Uur 2", "station_neutral": True, "local_news": False, "quality": QUALITY},
}
CLAIMS = {}      # code -> {user, used, expires}
TOKENS = {}      # token -> {user, revoked, device}
EPISODES = {}    # id -> {...}
DRAFTS = {}      # id -> {...}
FAIL = {"status": 0, "count": 0}
SEQ = {"ep": 380, "draft": 56, "dev": 6}
LOG = []


def now():
    return datetime.datetime.now().astimezone().isoformat(timespec="seconds")


def err(status, code, message="", details=None):
    return status, {"error": {"code": code, "message": message, "details": details or {}}}


def user_by_token(headers):
    auth = headers.get("Authorization", "")
    if not auth.startswith("Bearer "):
        return None
    t = TOKENS.get(auth[7:])
    if not t or t["revoked"]:
        return None
    return USERS[t["user"]]


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        LOG.append(f"{self.command} {self.path} -> {args[1] if len(args) > 1 else ''}")
        sys.stderr.write("%s %s\n" % (self.command, self.path))

    def send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body_bytes(self):
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            out = b""
            while True:
                line = self.rfile.readline().strip()
                n = int(line.split(b";")[0], 16)
                if n == 0:
                    self.rfile.readline()
                    break
                out += self.rfile.read(n)
                self.rfile.readline()
            return out
        n = int(self.headers.get("Content-Length", 0) or 0)
        return self.rfile.read(n) if n else b""

    def json_body(self):
        b = self.body_bytes()
        return json.loads(b) if b else {}

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")

    def do_PUT(self):
        self.route("PUT")

    def do_DELETE(self):
        self.route("DELETE")

    def route(self, method):
        try:
            with LOCK:
                status, body = self.handle_req(method)
        except Exception as e:  # pragma: no cover
            status, body = err(500, "SERVER_ERROR", str(e))
        self.send(status, body)

    def handle_req(self, m):
        p = self.path.split("?")[0]
        # ---- test helpers
        if p == "/test/claim-code" and m == "POST":
            b = self.json_body()
            code = "-".join("".join(secrets.choice("ABCDEFGHJKLMNPQRSTUVWXYZ23456789") for _ in range(4)) for _ in range(3))
            exp = (datetime.datetime.now().astimezone() + datetime.timedelta(days=b.get("days", 7))).isoformat(timespec="seconds")
            CLAIMS[code] = {"user": b["user"], "used": False, "expires": exp}
            host = self.headers.get("Host")
            return 200, {"code": code, "file": {"type": "audioshare-maker-link", "version": 1, "url": f"http://{host}",
                                                 "user": b["user"], "claim_code": code, "expires": exp}}
        if p == "/test/revoke" and m == "POST":
            b = self.json_body()
            for t in TOKENS.values():
                if t["user"] == b["user"]:
                    t["revoked"] = True
            return 200, {"ok": True}
        if p == "/test/fail-next" and m == "POST":
            b = self.json_body()
            FAIL.update(status=b.get("status", 503), count=b.get("count", 1))
            return 200, {"ok": True}
        if p == "/test/publish" and m == "POST":
            b = self.json_body()
            SEQ["ep"] += 1
            u = USERS[b["user"]]
            EPISODES[SEQ["ep"]] = {"id": SEQ["ep"], "program_id": b["program_id"], "broadcast_date": b["date"],
                                   "title": b.get("title", "Bestaande aflevering"), "description": "", "status": "published",
                                   "owner": u["id"], "revision": 1, "uploaded_at": now(),
                                   "downloaded_by": [{"station": "RTV Voorbeeld", "at": now(), "revision": 1}] if b.get("downloaded") else []}
            return 200, {"episode_id": SEQ["ep"]}
        if p == "/test/state":
            return 200, {"episodes": EPISODES, "drafts": {k: {kk: vv for kk, vv in v.items() if kk != "data"} for k, v in DRAFTS.items()},
                         "tokens": len(TOKENS), "log": LOG[-50:]}

        # ---- claim
        if p == "/api/v1/claim" and m == "POST":
            b = self.json_body()
            c = CLAIMS.get(b.get("claim_code", ""))
            if not c:
                return err(404, "INVALID_CLAIM_CODE", "Code bestaat niet of is ingetrokken")
            if c["used"]:
                return err(410, "CLAIM_CODE_USED", "Code is al gebruikt")
            if c["expires"] < now():
                return err(410, "CLAIM_CODE_EXPIRED", "Code is verlopen")
            c["used"] = True
            token = "as_mk_" + secrets.token_hex(12)
            SEQ["dev"] += 1
            TOKENS[token] = {"user": c["user"], "revoked": False, "device": b.get("device_name")}
            u = USERS[c["user"]]
            return 201, {"token": token, "user": {"id": u["id"], "display_name": u["display_name"]}, "device_id": SEQ["dev"]}

        if not p.startswith("/api/v1/me"):
            return err(404, "NOT_FOUND", "Onbekende route")
        if self.headers.get("X-API-Key"):
            return err(401, "INVALID_TOKEN", "Stationsleutels werken niet op /me")
        u = user_by_token(self.headers)
        if FAIL["count"] > 0:
            FAIL["count"] -= 1
            if m == "PUT":
                self.body_bytes()
            return err(FAIL["status"], "SERVER_ERROR", "Tijdelijke storing (test)")
        if not u:
            if m == "PUT":
                self.body_bytes()
            return err(401, "INVALID_TOKEN", "Token onbekend of apparaat ontkoppeld")

        if p == "/api/v1/me" and m == "GET":
            return 200, {"user": {k: u[k] for k in ("id", "username", "display_name")}, "permissions": ["downloaden", "programmas"], "server_time": now()}
        if p == "/api/v1/me/programs" and m == "GET":
            return 200, {"programs": [PROGRAMS[i] for i in u["programs"]]}

        mm = re.fullmatch(r"/api/v1/me/programs/(\d+)/upload-info", p)
        if mm and m == "GET":
            pid = int(mm.group(1))
            if pid not in u["programs"]:
                return err(403, "NOT_ASSIGNED", "Programma niet toegewezen")
            last = [e for e in EPISODES.values() if e["program_id"] == pid and e["status"] == "published"]
            last.sort(key=lambda e: e["broadcast_date"])
            return 200, {"suggested_title": f"{PROGRAMS[pid]['name']} — volgende week",
                         "suggested_description": last[-1]["description"] if last else "Omschrijving van de vorige aflevering…",
                         "last_broadcast_date": last[-1]["broadcast_date"] if last else None,
                         "subscribers": [{"station": "RTV Voorbeeld", "broadcast_time": "07:00"}]}

        mm = re.fullmatch(r"/api/v1/me/programs/(\d+)/episodes", p)
        if mm and m == "POST":
            pid = int(mm.group(1))
            if pid not in u["programs"]:
                return err(403, "NOT_ASSIGNED", "Programma niet toegewezen")
            b = self.json_body()
            for d in DRAFTS.values():
                if d["client_upload_id"] == b.get("client_upload_id") and d["owner"] == u["id"]:
                    return 200, self.draft_view(d)
            if not b.get("title") or not b.get("broadcast_date"):
                return err(400, "INVALID_INPUT", "titel en datum zijn verplicht")
            if b["broadcast_date"] < datetime.date.today().isoformat():
                return err(422, "INVALID_BROADCAST_DATE", "Datum ligt in het verleden")
            for e in EPISODES.values():
                if e["program_id"] == pid and e["broadcast_date"] == b["broadcast_date"] and e["status"] == "published":
                    owner = next(x for x in USERS.values() if x["id"] == e["owner"])
                    return err(409, "EPISODE_EXISTS", "Er staat al een aflevering op die datum",
                               {"episode_id": e["id"], "owner": owner["display_name"], "is_own": e["owner"] == u["id"]})
            SEQ["draft"] += 1
            d = {"id": SEQ["draft"], "kind": "new", "program_id": pid, "episode_id": None, "owner": u["id"],
                 "client_upload_id": b.get("client_upload_id"), "broadcast_date": b["broadcast_date"],
                 "title": b["title"], "description": b.get("description", ""), "parts": {1: None}, "finalized": None,
                 "operator": b.get("operator")}
            DRAFTS[d["id"]] = d
            return 201, self.draft_view(d)

        mm = re.fullmatch(r"/api/v1/me/drafts/(\d+)", p)
        if mm:
            d = DRAFTS.get(int(mm.group(1)))
            if not d or d["owner"] != u["id"]:
                return err(404, "NOT_FOUND", "Draft bestaat niet")
            if m == "GET":
                return 200, self.draft_view(d)
            if m == "DELETE":
                del DRAFTS[d["id"]]
                return 200, {"ok": True}

        mm = re.fullmatch(r"/api/v1/me/drafts/(\d+)/parts/(\d+)", p)
        if mm and m == "PUT":
            data = self.body_bytes()
            d = DRAFTS.get(int(mm.group(1)))
            if not d or d["owner"] != u["id"]:
                return err(404, "NOT_FOUND", "Draft bestaat niet")
            if d["finalized"]:
                return err(409, "ALREADY_FINALIZED", "Al vrijgegeven")
            if len(data) > 200 * 1024 * 1024:
                return err(413, "FILE_TOO_LARGE", "Deel > 200 MB")
            sha = hashlib.sha256(data).hexdigest()
            if sha != self.headers.get("X-Content-SHA256", ""):
                return err(422, "CHECKSUM_MISMATCH", "SHA-256 klopt niet")
            nr = int(mm.group(2))
            ctype = self.headers.get("Content-Type")
            if ctype not in ("audio/mpeg", "audio/flac", "audio/ogg"):
                return err(422, "QUALITY_CHECK_FAILED", f"Formaat {ctype} niet toegestaan", {"check": "format"})
            d["parts"][nr] = {"size": len(data), "sha256": sha, "filename": self.headers.get("X-Filename"), "ctype": ctype}
            return 200, {"nr": nr, "status": "uploaded", "size": len(data), "sha256": sha, "duration_sec": 0, "bitrate_kbps": 256}

        mm = re.fullmatch(r"/api/v1/me/drafts/(\d+)/finalize", p)
        if mm and m == "POST":
            self.json_body()
            d = DRAFTS.get(int(mm.group(1)))
            if not d or d["owner"] != u["id"]:
                return err(404, "NOT_FOUND", "Draft bestaat niet")
            if d["finalized"]:
                r = dict(d["finalized"]); r["already_finalized"] = True
                return 200, r
            if any(v is None for v in d["parts"].values()):
                return err(422, "PARTS_MISSING", "Niet alle delen zijn geüpload")
            if d["kind"] == "replace":
                e = EPISODES[d["episode_id"]]
                e["revision"] += 1
                e["title"] = d["title"] or e["title"]
                e["uploaded_at"] = now()
                res = {"episode_id": e["id"], "status": "published", "replaced": True, "revision": e["revision"], "warnings": []}
            else:
                SEQ["ep"] += 1
                EPISODES[SEQ["ep"]] = {"id": SEQ["ep"], "program_id": d["program_id"], "broadcast_date": d["broadcast_date"],
                                       "title": d["title"], "description": d["description"], "status": "published",
                                       "owner": d["owner"], "revision": 1, "uploaded_at": now(), "downloaded_by": []}
                res = {"episode_id": SEQ["ep"], "status": "published", "replaced": False, "revision": 1,
                       "warnings": [{"code": "LATE_DELIVERY", "message": "Je aflevering is na 06:45 aangeleverd (test).", "station": "RTV Voorbeeld"}]
                       if d["broadcast_date"] == datetime.date.today().isoformat() else []}
            d["finalized"] = res
            return 200, res

        mm = re.fullmatch(r"/api/v1/me/episodes/(\d+)/replace", p)
        if mm and m == "POST":
            b = self.json_body()
            e = EPISODES.get(int(mm.group(1)))
            if not e:
                return err(404, "NOT_FOUND", "Aflevering bestaat niet")
            if e["owner"] != u["id"]:
                owner = next(x for x in USERS.values() if x["id"] == e["owner"])
                return err(403, "NOT_OWNER", "Niet de eigenaar", {"owner": owner["display_name"], "episode_id": e["id"]})
            if not b.get("confirm"):
                return err(409, "CONFIRM_REQUIRED", "Deze aflevering bestaat al. Weet je zeker dat je hem wilt vervangen?",
                           {"episode_id": e["id"], "broadcast_date": e["broadcast_date"], "current_title": e["title"],
                            "uploaded_at": e["uploaded_at"], "downloaded_by": [{"station": x["station"], "at": x["at"]} for x in e["downloaded_by"]],
                            "deadline_passed": []})
            for d in DRAFTS.values():
                if d["client_upload_id"] == b.get("client_upload_id") and d["owner"] == u["id"]:
                    return 200, self.draft_view(d)
            SEQ["draft"] += 1
            d = {"id": SEQ["draft"], "kind": "replace", "program_id": e["program_id"], "episode_id": e["id"], "owner": u["id"],
                 "client_upload_id": b.get("client_upload_id"), "broadcast_date": e["broadcast_date"],
                 "title": b.get("title", ""), "description": b.get("description", ""), "parts": {1: None}, "finalized": None}
            DRAFTS[d["id"]] = d
            return 201, self.draft_view(d)

        return err(404, "NOT_FOUND", f"Onbekende route {m} {p}")

    @staticmethod
    def draft_view(d):
        return {"draft_id": d["id"], "kind": d["kind"], "program_id": d["program_id"], "episode_id": d["episode_id"],
                "status": "draft", "broadcast_date": d["broadcast_date"], "title": d["title"],
                "client_upload_id": d["client_upload_id"], "operator": d.get("operator"),
                "parts": [{"nr": k, "status": "uploaded" if v else "missing"} for k, v in d["parts"].items()]}


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    print(f"AudioShare mock on http://127.0.0.1:{port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
