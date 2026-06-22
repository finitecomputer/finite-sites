# /// script
# requires-python = ">=3.12"
# dependencies = ["python-fasthtml"]
# ///
# Idiomatic FastHTML with PEP 723 inline dependencies: `uv run app.py`
# resolves everything, so the published bundle is this single file.
import os
import sqlite3
from datetime import datetime, timezone

from fasthtml.common import *

DATA_DIR = os.environ.get("DATA_DIR", ".")
db = sqlite3.connect(f"{DATA_DIR}/guestbook.db", check_same_thread=False)
db.execute("CREATE TABLE IF NOT EXISTS entries (name TEXT, at TEXT)")

# The platform sandbox only allows writes under $DATA_DIR; FastHTML's
# session key lives there instead of the (read-only) code directory.
app, rt = fast_app(key_fname=f"{DATA_DIR}/.sesskey")


@rt("/")
def get():
    rows = db.execute("SELECT name, at FROM entries ORDER BY rowid DESC LIMIT 10").fetchall()
    return Titled(
        "FastHTML on Finite",
        P("A Python server app fixture for a future Project Output type."),
        Form(
            Input(name="name", placeholder="your name", required=True),
            Button("sign the guestbook"),
            method="post",
            action="/sign",
        ),
        Ul(*[Li(f"{name} — {at}") for name, at in rows]),
        P(f"server-rendered at {datetime.now(timezone.utc):%H:%M:%S} UTC"),
    )


@rt("/sign")
def post(name: str):
    safe = name.strip()[:80]
    if safe:
        db.execute("INSERT INTO entries VALUES (?, ?)", (safe, f"{datetime.now(timezone.utc):%Y-%m-%d %H:%M}"))
        db.commit()
    return Redirect("/")


serve(port=int(os.environ.get("PORT", 5001)))
