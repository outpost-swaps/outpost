import socket, struct, json, time, sys, statistics
def socks5_connect(host, port, proxy=('127.0.0.1', 61165), timeout=60):
    s = socket.create_connection(proxy, timeout=timeout)
    s.sendall(b'\x05\x01\x00'); assert s.recv(2) == b'\x05\x00'
    h = host.encode()
    s.sendall(b'\x05\x01\x00\x03' + bytes([len(h)]) + h + struct.pack('>H', port))
    rep = s.recv(10)
    if rep[1] != 0: raise OSError('socks error %d' % rep[1])
    return s
def call(s, buf, i, method, params):
    s.sendall((json.dumps({'id': i, 'method': method, 'params': params}) + '\n').encode())
    while b'\n' not in buf[0]:
        chunk = s.recv(65536)
        if not chunk: raise OSError('closed')
        buf[0] += chunk
    line, buf[0] = buf[0].split(b'\n', 1)
    return json.loads(line)
for name, host in [('endpoint0 user', 'xufljktdbo542e6jyc7hat6ybnxx6km57rhmw5qpbzyducdcofyx2kqd.onion'),
                   ('endpoint1 builtin', 'xnkshos5cd6skc3mdzwrfj6erddo24hm4myfsb6b6qyedvroptijg6id.onion')]:
    t0 = time.time()
    try:
        s = socks5_connect(host, 50001)
    except Exception as e:
        print(name, 'connect failed after %.1fs: %s' % (time.time() - t0, e)); continue
    tc = time.time() - t0
    buf = [b'']
    rtts = []
    for i in range(12):
        t = time.time()
        try:
            call(s, buf, i, 'server.ping', [])
        except Exception as e:
            print(name, 'ping failed:', e); break
        rtts.append(time.time() - t)
    hist = []
    for i in range(3):
        t = time.time()
        try:
            call(s, buf, 100 + i, 'blockchain.scripthash.get_history', ['%064x' % (i + 12345)])
        except Exception as e:
            print(name, 'history failed:', e); break
        hist.append(time.time() - t)
    if rtts:
        print('%-18s connect %.1fs | ping n=%d median %.2fs max %.2fs | history median %.2fs' % (
            name, tc, len(rtts), statistics.median(rtts), max(rtts), statistics.median(hist) if hist else -1))
    s.close()
