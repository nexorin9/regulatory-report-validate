#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""院内卫健上报网关的**协议兼容替身**（本机可控进程），仅供离线验证用。

它只做两件事：

1. `POST /api/v1/regulatory-submissions` —— 院内卫健上报网关的**出站写**端点。
   按 `spec.md`「出站报文（正式对接规范）」校验：
   - `Authorization: Bearer <token>` 头在；
   - `Idempotency-Key` 头在，且与报文里的 `idempotency_key` 同值；
   - body 字段全集正好是 15 项（不得少传多传）；
   - 合规则回 `200 {receipt_id, accepted_at, status}`，**按幂等键记账**：
     同一个键再来一次回的是同一张回执，不新建回执。
2. `GET /api/v1/dept-directory` —— 院内 HIS 科室主数据目录的**出站读**端点。
   回 `[{dept_code, dept_name, aliases[], parent_code}]`。

**它不是最终对接方式**：产品代码走的是上面这两条正式协议（路径 / 鉴权头 /
幂等头 / 字段全集都按规范来），替身只换了对端地址与响应内容。院内真实网关
与 HIS 的地址与凭据走环境变量，不在本文件里。

跑法（Python 3.8+，只用标准库）：

    python3 tests/gateway_stub/stub_server.py --port 8731 --token stub-token

另开一个终端把引擎指向它：

    export REGULATORY_REPORT_BASE_URL=http://127.0.0.1:8731
    export REGULATORY_REPORT_TOKEN=stub-token
    export HIS_BASE_URL=http://127.0.0.1:8731
    export HIS_API_TOKEN=stub-token
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# ── 出站报文（正式对接规范）里的 15 项 body 字段全集 ──────────────────────
SUBMISSION_BODY_FIELDS = (
    "batch_ref",
    "reporting_period",
    "org_code",
    "indicator_code",
    "dept_code",
    "business_key",
    "numerator",
    "denominator",
    "value",
    "unit",
    "basis",
    "rule_version",
    "idempotency_key",
    "payload_digest",
    "revision",
)

# ── 科室目录条目字段全集 ──────────────────────────────────────────────────
DEPT_FIELDS = ("dept_code", "dept_name", "aliases", "parent_code")

# 替身自带的科室目录样例（脱敏：科室代码与名称均为示意，非真实科室）
DEPT_DIRECTORY = [
    {"dept_code": "0301", "dept_name": "病案统计室", "aliases": ["病案统计"], "parent_code": "0300"},
    {"dept_code": "0302", "dept_name": "病案室", "aliases": ["病案科"], "parent_code": "0300"},
    {"dept_code": "0303", "dept_name": "医技部", "aliases": ["医技科"], "parent_code": "0300"},
    {"dept_code": "0304", "dept_name": "质控科", "aliases": [], "parent_code": "0300"},
    {"dept_code": "0305", "dept_name": "院感科", "aliases": ["医院感染管理科"], "parent_code": "0300"},
    {"dept_code": "0306", "dept_name": "医务科", "aliases": ["医务处"], "parent_code": "0300"},
    {"dept_code": "0307", "dept_name": "信息科", "aliases": ["信息管理科"], "parent_code": "0300"},
    {"dept_code": "0308", "dept_name": "护理部", "aliases": [], "parent_code": "0300"},
]


class _State:
    """替身的记账本：幂等键 → 回执；收到的请求流水。"""

    def __init__(self, log_path: str = "") -> None:
        self.lock = threading.Lock()
        self.receipts: dict[str, dict] = {}
        self.requests: list[dict] = []
        self.counter = 0
        self.log_path = log_path

    def accept(self, idempotency_key: str) -> dict:
        with self.lock:
            hit = self.receipts.get(idempotency_key)
            if hit is not None:
                return hit
            self.counter += 1
            receipt = {
                "receipt_id": "RCPT-%04d" % self.counter,
                "accepted_at": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
                "status": "accepted",
            }
            self.receipts[idempotency_key] = receipt
            return receipt

    def record(self, entry: dict) -> None:
        with self.lock:
            self.requests.append(entry)
            if not self.log_path:
                return
            # 收到一条就落一条（不等进程退出），方便跑一半时也查得到网关收到了什么
            with open(self.log_path, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(entry, ensure_ascii=False) + "\n")


def _reject(msg: str, code: str = "E_STUB_REJECTED") -> tuple[int, dict]:
    return 400, {"error_code": code, "error_field": "", "message": msg}


class Handler(BaseHTTPRequestHandler):
    server_version = "regval-stub/1.0"
    protocol_version = "HTTP/1.1"

    # ── 工具 ──────────────────────────────────────────────────────────────
    def _send(self, status: int, payload: dict) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _bearer(self) -> str:
        header = self.headers.get("Authorization", "")
        if not header.startswith("Bearer "):
            return ""
        return header[len("Bearer "):].strip()

    def _authorized(self) -> bool:
        token = self._bearer()
        return bool(token) and token == self.server.expected_token

    def log_message(self, fmt: str, *args) -> None:  # noqa: A003 —— 静音默认访问日志
        if self.server.verbose:
            sys.stderr.write("[stub] " + (fmt % args) + "\n")

    # ── 出站写：上报网关 ───────────────────────────────────────────────────
    def do_POST(self) -> None:  # noqa: N802 —— BaseHTTPRequestHandler 的约定名
        state: _State = self.server.state
        if self.path != "/api/v1/regulatory-submissions":
            self._send(404, {"error_code": "E_NOT_FOUND", "error_field": "", "message": "没有这个端点"})
            return
        length = int(self.headers.get("Content-Length", "0") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            body = json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            status, payload = _reject("报文不是合法 JSON：%s" % exc)
            self._send(status, payload)
            return

        idem_header = self.headers.get("Idempotency-Key", "").strip()
        idem_body = str(body.get("idempotency_key", "") or "").strip()

        # 逐条校验：顺序固定，便于对照 spec.md 的报文规范
        if not self._authorized():
            status, payload = _reject("缺少或写错了 Authorization 头（应为 Bearer 凭据）", "E_UNAUTHORIZED")
        elif not idem_header:
            status, payload = _reject("缺少 Idempotency-Key 头", "E_IDEMPOTENCY_KEY_MISSING")
        elif idem_header != idem_body:
            status, payload = _reject(
                "Idempotency-Key 头与报文里的 idempotency_key 不一致（头 %s / 报文 %s）"
                % (idem_header, idem_body),
                "E_IDEMPOTENCY_KEY_MISMATCH",
            )
        else:
            missing = [f for f in SUBMISSION_BODY_FIELDS if f not in body]
            extra = [f for f in body if f not in SUBMISSION_BODY_FIELDS]
            if missing:
                status, payload = _reject("报文少传字段：%s" % "、".join(missing), "E_FIELD_MISSING")
            elif extra:
                status, payload = _reject("报文多传字段：%s" % "、".join(extra), "E_FIELD_UNEXPECTED")
            else:
                status, payload = 200, state.accept(idem_header)

        state.record(
            {
                "method": "POST",
                "path": self.path,
                "idempotency_key": idem_header,
                "status": status,
                "body": body,
            }
        )
        self._send(status, payload)

    # ── 出站读：HIS 科室主数据目录 ─────────────────────────────────────────
    def do_GET(self) -> None:  # noqa: N802
        state: _State = self.server.state
        if self.path != "/api/v1/dept-directory":
            self._send(404, {"error_code": "E_NOT_FOUND", "error_field": "", "message": "没有这个端点"})
            return
        if not self._authorized():
            self._send(
                401,
                {"error_code": "E_UNAUTHORIZED", "error_field": "", "message": "缺少或写错了 Authorization 头"},
            )
            return
        state.record({"method": "GET", "path": self.path, "status": 200, "body": None})
        self._send(200, DEPT_DIRECTORY)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="院内卫健上报网关 / HIS 科室目录的协议兼容替身")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8731)
    ap.add_argument("--token", default="stub-token", help="替身期待的 Bearer 凭据")
    ap.add_argument("--receipt-log", default="", help="把收到的请求流水追加写到这个 JSONL 文件（收到一条写一条）")
    ap.add_argument("--verbose", action="store_true", help="打印访问日志")
    args = ap.parse_args(argv)

    state = _State(args.receipt_log)
    httpd = ThreadingHTTPServer((args.host, args.port), Handler)
    httpd.state = state  # type: ignore[attr-defined]
    httpd.expected_token = args.token  # type: ignore[attr-defined]
    httpd.verbose = args.verbose  # type: ignore[attr-defined]

    print("替身已起：http://%s:%d" % (args.host, args.port), flush=True)
    print("  出站写 POST /api/v1/regulatory-submissions", flush=True)
    print("  出站读 GET  /api/v1/dept-directory", flush=True)
    print("  Bearer 凭据：%s" % args.token, flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        if args.receipt_log:
            with open(args.receipt_log, "w", encoding="utf-8") as fh:
                for entry in state.requests:
                    fh.write(json.dumps(entry, ensure_ascii=False) + "\n")
        httpd.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
