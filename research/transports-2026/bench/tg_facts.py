#!/usr/bin/env python3
"""Pull the load-bearing sentences out of the live Telegram Bot API page.

The report quotes these verbatim, so they are extracted mechanically rather
than retyped. Run:

    python3 research/transports-2026/bench/tg_facts.py
"""

import html
import re
import urllib.request

URL = "https://core.telegram.org/bots/api"

NEEDLES = [
    ("markdownv2 escape rule", "In all other places characters", 260),
    ("markdownv2 backslash", "Any character with code between 1 and 126", 300),
    ("markdownv2 pre/code", "Inside pre and code entities", 160),
    ("markdownv2 link", "Inside the (...) part of the inline link", 180),
    ("sendMessage length", "1-4096 characters after entities parsing", 60),
    ("entity offsets", "UTF-16 code units to the start", 200),
    ("retry_after", "In case of exceeding flood control", 130),
    ("getUpdates offset", "Must be greater by one than the highest", 220),
    ("getUpdates vs webhook", "This method will not work if an outgoing webhook", 90),
    ("update retention", "they will not be kept longer than 24 hours", 60),
    ("webhook secret_token", "A secret token to be sent in a header", 200),
    ("webhook ports", "Ports currently supported for webhooks", 60),
    ("callback_data", "Data to be sent in a callback query", 130),
    ("editMessageText", "Use this method to edit text and game messages", 120),
    ("sendChatAction", "when a message arrives from your bot, Telegram clients clear", 200),
]


def main():
    req = urllib.request.Request(URL, headers={"User-Agent": "jod-research/0.1"})
    with urllib.request.urlopen(req, timeout=60) as fh:
        raw = fh.read().decode("utf-8", "replace")
    text = html.unescape(re.sub(r"<[^>]+>", " ", raw))
    text = re.sub(r"[ \t]+", " ", text)
    for label, needle, span in NEEDLES:
        i = text.find(needle)
        print(f"\n## {label}")
        print("NOT FOUND — the page changed" if i < 0 else text[i : i + span].strip())


if __name__ == "__main__":
    main()
