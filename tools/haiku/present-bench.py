#!/usr/bin/env python3
"""Run on the target: what getting frames onto the screen costs.

Times two kinds of frame through PhotoCraft's opt-in control server: a change
of the whole canvas (a layer's opacity) and a small one (the pointer moving
between two toolbar buttons, so one hover highlight changes). Each sample is
the request-to-next-frame latency; run PhotoCraft with
RUST_LOG=warn,photocraft_haiku=debug to also log each frame's present cost.
"""
import argparse
import json
from pathlib import Path
import socket
import statistics
import time


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--token-file', type=Path, required=True)
    p.add_argument('--port', type=int, default=7878)
    p.add_argument('--samples', type=int, default=20)
    p.add_argument('--width', type=int, default=1000)
    p.add_argument('--height', type=int, default=700)
    args = p.parse_args()
    with socket.create_connection(('127.0.0.1', args.port), timeout=60) as conn:
        io = conn.makefile('rwb')
        seq = 0

        def call(method, params=None):
            nonlocal seq
            seq += 1
            io.write((json.dumps({'id': seq, 'method': method, 'params': params or {}}) + '\n').encode())
            io.flush()
            r = json.loads(io.readline(32 << 20))
            if not r.get('ok'):
                raise RuntimeError(f'{method}: {r.get("error")}')
            return r.get('result')

        def command(command_id, **params):
            return call('engine.execute', {'command': command_id, 'params': params})

        def timed(step):
            samples = []
            for i in range(args.samples + 2):
                started = time.perf_counter()
                step(i)
                # Answered in the next frame, after the changed one was presented.
                call('ui.inspect')
                if i >= 2:
                    samples.append((time.perf_counter() - started) * 1000)
            return {'medianMs': round(statistics.median(samples), 2), 'minMs': round(min(samples), 2),
                    'maxMs': round(max(samples), 2)}

        call('auth', {'token': args.token_file.read_text().strip()})
        call('ui.resize', {'width': args.width, 'height': args.height})
        command('file.new', width=3000, height=2000, depth=8, background='#ddeeff', name='Present benchmark')
        command('layer.new.layer', name='Colour')
        command('edit.fill', color='#c67429')
        command('view.fitOnScreen')
        call('ui.inspect')
        report = {'window': [args.width, args.height]}
        report['canvas'] = timed(lambda i: command('layer.setProps', opacity=0.55 if i % 2 else 0.85))
        # Two tool buttons in the left toolbar (logical window coordinates).
        report['hover'] = timed(lambda i: call('ui.move', {'x': 14, 'y': 60 if i % 2 else 84}))
        report['gpuInfo'] = call('ui.inspect')['perf']['timings']['gpuInfo']
        print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
