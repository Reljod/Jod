#!/usr/bin/env python3
"""Measure real TCP latency from this machine to each provider's network.

Marketing pages claim "premium bandwidth" and "low latency" universally, which
makes the claim worthless. This measures it instead.

Method: TCP connect time to port 443, several samples, median reported. TCP
rather than ICMP because ping is widely rate-limited or dropped at the edge,
and because a completed TCP handshake is closer to what Jod actually does.

Caveats, stated plainly:
  - This measures latency to each provider's *public web endpoint*, which is
    usually corporate hosting or a CDN, not the datacenter you would buy in.
    Treat it as a coarse reachability signal, not a datacenter benchmark.
  - Results depend on where you run it. Run it from your own network.
  - A provider fronted by Cloudflare will look artificially fast.
  Endpoints marked `cdn: true` are flagged in the output for this reason.

Usage:
    python3 netcheck.py
    python3 netcheck.py --samples 7 --json ../out/netcheck.json
"""

import argparse
import json
import os
import socket
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "out")

# provider id -> (hostname, behind_a_cdn)
ENDPOINTS = {
    "hetzner": ("hetzner.com", False),
    "netcup": ("www.netcup.com", False),
    "contabo": ("contabo.com", True),
    "ovh": ("www.ovhcloud.com", True),
    "ionos": ("www.ionos.com", True),
    "scaleway": ("www.scaleway.com", True),
    "time4vps": ("www.time4vps.com", True),
    "webdock": ("webdock.io", True),
    "liteserver": ("www.liteserver.nl", False),
    "zomro": ("zomro.com", True),
    "vdsina": ("vdsina.com", True),
    "pqhosting": ("pq.hosting", True),
    "gcore": ("gcore.com", True),
    "upcloud": ("upcloud.com", True),
    "exoscale": ("www.exoscale.com", True),
    "hostkey": ("hostkey.com", True),
    "digitalocean": ("www.digitalocean.com", True),
    "vultr": ("www.vultr.com", True),
    "linode": ("www.linode.com", True),
    "lightsail": ("aws.amazon.com", True),
    "gcp": ("cloud.google.com", True),
    "azure": ("azure.microsoft.com", True),
    "oracle": ("www.oracle.com", True),
    "alibaba": ("www.alibabacloud.com", True),
    "tencent": ("www.tencentcloud.com", True),
    "civo": ("www.civo.com", True),
    "kamatera": ("www.kamatera.com", True),
    "hivelocity": ("www.hivelocity.net", True),
    "phoenixnap": ("phoenixnap.com", True),
    "latitude": ("www.latitude.sh", True),
    "racknerd": ("www.racknerd.com", False),
    "buyvm": ("buyvm.net", True),
    "hosthatch": ("hosthatch.com", True),
    "greencloud": ("greencloudvps.com", True),
    "servarica": ("servarica.com", False),
    "advinservers": ("advinservers.com", True),
    "virmach": ("virmach.com", True),
    "interserver": ("www.interserver.net", False),
    "hostwinds": ("www.hostwinds.com", True),
    "vpsdime": ("vpsdime.com", False),
    "cloudzy": ("cloudzy.com", True),
    "hostinger": ("www.hostinger.com", True),
    "scalahosting": ("www.scalahosting.com", True),
    "ultahost": ("ultahost.com", True),
    "flokinet": ("flokinet.is", False),
    "abelohost": ("abelohost.com", True),
    "shinjiru": ("shinjiru.com", True),
    "orangewebsite": ("www.orangewebsite.com", False),
    "1984hosting": ("1984.hosting", False),
    "njalla": ("njal.la", False),
    "m247": ("www.m247.com", True),
    "hostslick": ("hostslick.com", True),
    "incognet": ("incognet.io", True),
    "prq": ("prq.se", False),
    "melbicom": ("www.melbicom.net", True),
    "vpsserver": ("www.vpsserver.com", True),
    "ovhkimsufi": ("www.kimsufi.com", True),
    "flyio": ("fly.io", True),
    "railway": ("railway.app", True),
}

PORT = 443
TIMEOUT = 6.0


def tcp_rtt_ms(host, port=PORT, timeout=TIMEOUT):
    """One TCP handshake, in milliseconds. None if it fails."""
    try:
        info = socket.getaddrinfo(host, port, proto=socket.IPPROTO_TCP)[0]
    except socket.gaierror:
        return None, "dns-failure"

    family, socktype, proto, _, sockaddr = info
    s = socket.socket(family, socktype, proto)
    s.settimeout(timeout)
    try:
        start = time.perf_counter()
        s.connect(sockaddr)
        elapsed = (time.perf_counter() - start) * 1000.0
        return elapsed, None
    except (socket.timeout, TimeoutError):
        return None, "timeout"
    except OSError as e:
        return None, f"error: {e.__class__.__name__}"
    finally:
        s.close()


def measure(pid, host, cdn, samples):
    rtts, err = [], None
    for _ in range(samples):
        rtt, e = tcp_rtt_ms(host)
        if rtt is not None:
            rtts.append(rtt)
        else:
            err = e
        time.sleep(0.05)

    if not rtts:
        return pid, {"host": host, "cdn": cdn, "ok": False, "error": err or "unreachable"}

    return pid, {
        "host": host,
        "cdn": cdn,
        "ok": True,
        "median_ms": round(statistics.median(rtts), 1),
        "min_ms": round(min(rtts), 1),
        "jitter_ms": round(max(rtts) - min(rtts), 1),
        "loss_pct": round(100.0 * (samples - len(rtts)) / samples, 1),
    }


def main():
    ap = argparse.ArgumentParser(description="Measure TCP latency to provider endpoints.")
    ap.add_argument("--samples", type=int, default=5)
    ap.add_argument("--workers", type=int, default=12)
    ap.add_argument("--json", default=os.path.join(OUT, "netcheck.json"))
    args = ap.parse_args()

    print(f"measuring {len(ENDPOINTS)} endpoints, {args.samples} samples each ...\n")

    results = {}
    with ThreadPoolExecutor(max_workers=args.workers) as ex:
        futures = [
            ex.submit(measure, pid, host, cdn, args.samples)
            for pid, (host, cdn) in ENDPOINTS.items()
        ]
        for fut in futures:
            pid, res = fut.result()
            results[pid] = res

    ok = {k: v for k, v in results.items() if v["ok"]}
    bad = {k: v for k, v in results.items() if not v["ok"]}

    print(f"{'provider':<18} {'median':>9} {'jitter':>8}   note")
    print("-" * 52)
    for pid, r in sorted(ok.items(), key=lambda kv: kv[1]["median_ms"]):
        note = "cdn-fronted" if r["cdn"] else ""
        print(f"{pid:<18} {r['median_ms']:>7.1f}ms {r['jitter_ms']:>6.1f}ms   {note}")

    if bad:
        print(f"\nunreachable ({len(bad)}):")
        for pid, r in bad.items():
            print(f"  {pid:<18} {r['error']}")

    os.makedirs(os.path.dirname(args.json), exist_ok=True)
    with open(args.json, "w") as fh:
        json.dump(
            {
                "measured_from": socket.gethostname(),
                "samples": args.samples,
                "port": PORT,
                "caveat": "Latency to public web endpoints, not to purchasable datacenters. CDN-fronted hosts appear faster than their metal.",
                "results": results,
            },
            fh,
            indent=2,
        )
    print(f"\nwrote {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
