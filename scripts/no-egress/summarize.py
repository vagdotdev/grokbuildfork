#!/usr/bin/env python3
"""Summarize an observed run: hostnames from the proxy log and DNS query payloads, plus connect() targets."""
import re, sys, os, collections

outdir = sys.argv[1]
label_re = re.compile(r'((?:\\\d{1,3}[A-Za-z0-9_-]+){2,})\\0')
addr4_re = re.compile(r'connect\(\d+, \{sa_family=AF_INET, sin_port=htons\((\d+)\), sin_addr=inet_addr\("([^"]+)"\)')
addr6_re = re.compile(r'connect\(\d+, \{sa_family=AF_INET6, sin6_port=htons\((\d+)\),[^}]*inet_pton\(AF_INET6, "([^"]+)"')
unix_re = re.compile(r'connect\(\d+, \{sa_family=AF_UNIX, sun_path="([^"]+)"')

dns = collections.Counter()
connects = collections.Counter()
unix = collections.Counter()
with open(os.path.join(outdir, "strace.log"), errors="replace") as f:
    for line in f:
        if "sendto(" in line or "sendmsg(" in line or "connect(" in line:
            for m in label_re.finditer(line):
                labels = re.findall(r'\\\d{1,3}([A-Za-z0-9_-]+)', m.group(1))
                if labels:
                    dns[".".join(labels)] += 1
        m = addr4_re.search(line)
        if m:
            connects["%s:%s" % (m.group(2), m.group(1))] += 1
        m = addr6_re.search(line)
        if m:
            connects["[%s]:%s" % (m.group(2), m.group(1))] += 1
        m = unix_re.search(line)
        if m:
            unix[m.group(1)] += 1

proxy = collections.Counter()
p = os.path.join(outdir, "proxy.log")
if os.path.exists(p):
    for line in open(p):
        parts = line.split()
        if len(parts) >= 3 and parts[1] != "ERROR":
            proxy[parts[2]] += 1

print("== proxy (exact hostnames that honored HTTP(S)_PROXY) ==")
for k, v in proxy.most_common(): print("  %5d  %s" % (v, k))
if not proxy: print("  (none)")
print("== DNS query names seen in sendto/sendmsg payloads ==")
for k, v in dns.most_common(): print("  %5d  %s" % (v, k))
if not dns: print("  (none)")
print("== connect() targets (AF_INET/AF_INET6) ==")
for k, v in connects.most_common(): print("  %5d  %s" % (v, k))
if not connects: print("  (none)")
print("== connect() AF_UNIX ==")
for k, v in unix.most_common(): print("  %5d  %s" % (v, k))

forbidden = re.compile(r'(x\.ai|grok\.com|mixpanel\.com|googleapis\.com)$|(x\.ai|grok\.com|mixpanel\.com|googleapis\.com)[:/]', re.I)
bad = [k for k in list(proxy) + list(dns) if forbidden.search(k)]
print("== forbidden hosts (x.ai / grok.com / mixpanel / googleapis) ==")
print("  " + (", ".join(sorted(set(bad))) if bad else "NONE"))
sys.exit(1 if bad else 0)
