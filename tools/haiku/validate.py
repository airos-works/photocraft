#!/usr/bin/env python3
"""Run on the target: 24 MP compositor benchmark and simulated GPU-loss check.

Requires PhotoCraft's opt-in control server with an automation write root. The
report contains local command-to-next-frame latency, not GPU-only kernel time.
Creates synthetic test documents and changes the test instance's GPU preference.
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
    p.add_argument('--report', type=Path, required=True)
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

        call('auth', {'token': args.token_file.read_text().strip()})
        call('ui.resize', {'width': 1100, 'height': 720})
        command('file.new', width=6000, height=4000, depth=8, background='#ddeeff', name='24 MP performance')
        command('layer.new.layer', name='Full-size colour')
        command('edit.fill', color='#c67429')
        command('layer.setProps', opacity=0.55)
        command('layer.new.layer', name='Paint', mode='Multiply')
        command('paint.stroke', points=[[600,800,1],[2200,2800,1],[4200,900,1],[5500,3300,1]], size=1000, hardness=0.4, color='#23bbaa', seed=19)
        command('view.fitOnScreen')
        call('app.save', {'path': 'benchmark-24mp.pcraft'})
        report = {'size': [6000, 4000], 'depth': 8, 'layers': 3, 'warmups': 2, 'samples': 8}

        def benchmark(mode):
            samples = []
            for i in range(10):
                started = time.perf_counter()
                command('layer.setProps', opacity=0.55 if i % 2 else 0.85)
                # This request runs in the next frame, after the changed frame's
                # compositor, window renderer, GPU readback and presentation.
                inspect = call('ui.inspect')
                elapsed = (time.perf_counter() - started) * 1000
                timing = inspect['perf']['timings']
                if mode == 'vulkan' and not timing['gpu']:
                    raise RuntimeError(f'GPU canvas fell back: {timing}')
                if i >= 2:
                    samples.append({'endToEndMs': elapsed, 'compositeMs': timing['compositeMs'], 'uploadMs': timing['uploadMs']})
            report[mode] = {'samples': samples, 'medianMs': statistics.median(s['endToEndMs'] for s in samples), 'gpuInfo': timing['gpuInfo']}
            call('ui.screenshot', {'path': f'benchmark-24mp-{mode}.png', 'focus': False})
            args.report.write_text(json.dumps(report, indent=2) + '\n')

        benchmark('vulkan')
        call('app.save', {'path': 'benchmark-24mp.pcraft'})
        report['simulatedLoss'] = call('ui.gpu.simulateLoss')
        call('ui.inspect')
        # Default English recovery dialog, centered in the 1100x720 test window.
        call('ui.click', {'x': 422, 'y': 442})
        call('ui.inspect')
        inspect = call('ui.inspect')
        if inspect['gpuFallbackNotice'] is not None:
            raise RuntimeError('Dismiss Keep Using CPU before benchmarking the CPU renderer')
        report['afterLoss'] = inspect['perf']['timings']['gpuInfo']
        if report['afterLoss']['canvas'] != 'cpu' or report['afterLoss']['present'] != 'native software bitmap':
            raise RuntimeError(f'Native renderer did not fall back: {report["afterLoss"]}')
        benchmark('cpu')
        call('app.save', {'path': 'benchmark-24mp-after-loss.pcraft'})
        report['speedup'] = report['cpu']['medianMs'] / report['vulkan']['medianMs']
        args.report.write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({m: report[m]['medianMs'] for m in ['vulkan', 'cpu']}, indent=2))
        print('CPU/Vulkan latency ratio:', report['speedup'])


if __name__ == '__main__':
    main()
