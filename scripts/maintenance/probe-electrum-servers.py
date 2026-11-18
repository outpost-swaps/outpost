import json, socket, struct, ssl, sys, time, hashlib, threading, os
GENESIS = {'mainnet': '000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f',
           'testnet3': '000000000933ea01ad0ee984209779baaec3ced90fa3f408719526f8d77f4943',
           'testnet4': '00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043'}
def socks(host, port, timeout):
    s = socket.create_connection(('127.0.0.1', 9050), timeout=timeout)
    s.sendall(b'\x05\x01\x00'); assert s.recv(2) == b'\x05\x00'
    h = host.encode(); s.sendall(b'\x05\x01\x00\x03' + bytes([len(h)]) + h + struct.pack('>H', port))
    rep = b''
    while len(rep) < 10:
        c = s.recv(10 - len(rep))
        if not c: raise OSError('socks closed')
        rep += c
    if rep[1] != 0: raise OSError('socks rep %d' % rep[1])
    return s
def probe(url, timeout=75):
    r = {'url': url, 'ok': False}
    try:
        scheme, rest = url.split('://'); host, port = rest.rsplit(':', 1)
        t0 = time.time(); s = socks(host, int(port), timeout)
        if scheme == 'ssl':
            ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
            s = ctx.wrap_socket(s, server_hostname=host)
        r['connect'] = round(time.time() - t0, 1); s.settimeout(timeout); buf = b''
        def call(i, m, p):
            nonlocal buf
            s.sendall((json.dumps({'id': i, 'method': m, 'params': p}) + '\n').encode())
            while b'\n' not in buf:
                c = s.recv(65536)
                if not c: raise OSError('closed')
                buf += c
            line, buf = buf.split(b'\n', 1)
            return json.loads(line)
        v = call(1, 'server.version', ['probe', '1.4']); r['version'] = str(v.get('result'))[:40]
        h = call(2, 'blockchain.headers.subscribe', []); r['height'] = h['result']['height']
        g = call(3, 'blockchain.block.header', [0])['result']
        raw = bytes.fromhex(g[:160]); r['genesis'] = hashlib.sha256(hashlib.sha256(raw).digest()).digest()[::-1].hex()
        r['ok'] = True; s.close()
    except Exception as e:
        r['err'] = (type(e).__name__ + ':' + str(e))[:40]
    return r
cands = json.load(open(os.environ['TMPDIR'] + '/cands.json'))
results = []; lock = threading.Lock()
def work(c):
    r = probe(c['url']); r.update({k: c[k] for k in ('src', 'net', 'name')})
    with lock: results.append(r)
threads = [threading.Thread(target=work, args=(c,)) for c in cands]
for t in threads: t.start()
for t in threads: t.join(timeout=100)
json.dump(results, open(os.environ['TMPDIR'] + '/probe_results.json', 'w'))
print('probed', len(results), 'of', len(cands))
