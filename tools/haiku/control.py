#!/usr/bin/env python3
"""Drive an opt-in PhotoCraft control server on the same device.

Feed JSON requests on stdin; credentials are read only from the token file.
An optional client-only `_select` array narrows top-level result fields in output.
"""
import argparse
import json
import socket
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--port', type=int, default=7878)
    parser.add_argument('--token-file', type=Path, required=True)
    parser.add_argument('--timeout', type=float, default=60)
    args = parser.parse_args()
    import sys
    with socket.create_connection(('127.0.0.1', args.port), args.timeout) as conn:
        io = conn.makefile('rwb')
        def call(request):
            io.write((json.dumps(request) + '\n').encode())
            io.flush()
            line = io.readline(32 * 1024 * 1024)
            if not line:
                raise RuntimeError('PhotoCraft closed the control connection')
            return json.loads(line)
        reply = call({'id': 'auth', 'method': 'auth', 'params': {'token': args.token_file.read_text().strip()}})
        if not reply.get('ok'):
            raise RuntimeError('PhotoCraft authentication failed')
        for n, line in enumerate(sys.stdin, 1):
            if not line.strip():
                continue
            request = json.loads(line)
            select = request.pop('_select', None)
            request.setdefault('id', n)
            result = call(request)
            if select and isinstance(result.get('result'), dict):
                result['result'] = {key: result['result'].get(key) for key in select}
            print(json.dumps(result), flush=True)
            if not result.get('ok'):
                return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
