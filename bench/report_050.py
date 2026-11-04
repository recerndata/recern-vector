"""Validate and summarize Vector 0.5 measurements; plotting is optional.

--check-persistence FILE is stdlib-only and used by CI's small smoke run.
The full report requires three complete raw reports; --plots needs matplotlib.
"""
import argparse
import hashlib
import itertools
import json
import math
from pathlib import Path
import statistics


def load(path):
    data=json.loads(Path(path).read_text())
    assert data.get('complete') is True, f'incomplete report: {path}'
    assert data['method']['profile']=='release'
    return data


def check_persistence(data):
    method=data['method']; rows=data['results']
    current={r['case']['version'] for r in rows if r['case']['version']!='0.4.0'}
    assert len(current)==1
    current=current.pop()
    candidates={(current,3),(current,2),('0.4.0',2)}
    expected=set(itertools.product(method['sizes'],['f32','int8'],candidates,range(method['builds_per_case'])))
    actual=set()
    for r in rows:
        c=r['case'];key=(c['records'],c['precision'],(c['version'],c['format']),c['repeat'])
        assert key not in actual, 'duplicate persistence case'
        actual.add(key)
        assert r['peak_rss_bytes']>0
        for name,phase in r['phases'].items():
            assert len(phase['samples_ms'])==method['commits_per_phase']
            assert all(math.isfinite(v) and v>0 for v in phase['samples_ms'])
            if name=='no_op': assert set(phase['wal_growth_bytes'])=={0}
            if c['format']==3 and name in ('delete','no_op'):
                records,pages=(1,2) if name=='delete' else (0,0)
                assert all(s['last_encoded_records']==records and s['last_written_pages']==pages and s['last_validated_bytes']==4124 for s in phase['save_work'])
        if c['format']==3:
            assert r['snapshot_bytes_before_mutations']==(c['records']+2)*4096+4
    assert actual==expected, f'missing/extra persistence cases: {expected ^ actual}'
    return current


def summaries(data):
    groups={}
    for r in data['results']:
        c=r['case'];groups.setdefault((c['records'],c['precision'],c['version'],c['format']),[]).append(r)
    return [dict(records=k[0],precision=k[1],version=k[2],format=k[3],
                 peak_rss_bytes=statistics.median(r['peak_rss_bytes'] for r in rs),
                 snapshot_bytes=statistics.median(r['snapshot_bytes_before_mutations'] for r in rs),
                 vector_bytes=statistics.median(r['collection_stats']['vector_bytes'] for r in rs),
                 phases={op:{key:statistics.median(r['phases'][op][key] for r in rs) for key in ('p50_ms','p95_ms')}
                         for op in ('update','delete','no_op')}) for k,rs in sorted(groups.items())]


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--check-persistence',type=Path)
    p.add_argument('--reports',type=Path,default=Path('bench/releases/0.5.0-dev.1'))
    p.add_argument('--plots',action='store_true')
    a=p.parse_args()
    if a.check_persistence:
        data=load(a.check_persistence);check_persistence(data)
        print(f"Validated {len(data['results'])} isolated persistence cases")
        return
    folder=a.reports;persist=load(folder/'persistence.json');current=check_persistence(persist)
    ann=[load(folder/f'{name}.json') for name in ('sift','glove')]
    expected_sizes={'sift':(1000000,128,'l2'),'glove':(1183514,100,'cosine')}
    native={r['package']['native_sha256'] for r in persist['results'] if r['case']['version']==current}
    assert len(native)==1
    for d in ann:
        assert d['scope']=='full ANN dataset'
        assert d['source']==persist['source'], 'source changed during measurements'
        assert {r['case']['precision'] for r in d['results']}=={'f32','int8'}
        assert d['method']['queries']==1000 and d['method']['query_repeats']==3
        for r in d['results']:
            assert r['package']['native_sha256'] in native
            assert (r['records'],r['dim'],r['metric'])==expected_sizes[d['dataset']['name']]
            assert [v['ef'] for v in r['runs']]==d['method']['ef_sweep']
            assert all(0<=v['recall']<=1 and v['qps']>0 and len(v['repetitions'])==3 for v in r['runs'])
    rows=summaries(persist)
    (folder/'summary.json').write_text(json.dumps({'current':current,'aggregation':'median of three per-process p50/p95/high-water measurements','persistence':rows},indent=2)+'\n')
    text=['# Vector 0.5.0-dev.1 results','',
          'See [method, environment and commands](README.md). Save scheduling is disabled; these are warm-cache, single-process observations, not serving SLOs. Raw repetitions are retained.','',
          '## Persistence','',
          'Latency is save-only. Each cell is the median of three per-process medians; peak RSS includes the complete process and all benchmark phases.','',
          '| Records | Precision | Engine / format | Update p50 ms | Delete p50 ms | Clean-save p50 ms | Peak RSS MiB | Snapshot MiB |',
          '|---:|---|---|---:|---:|---:|---:|---:|']
    for r in rows:
        phases=r['phases']
        text.append(f"| {r['records']:,} | {r['precision']} | {r['version']} / {r['format']} | {phases['update']['p50_ms']:.3f} | {phases['delete']['p50_ms']:.3f} | {phases['no_op']['p50_ms']:.3f} | {r['peak_rss_bytes']/2**20:.1f} | {r['snapshot_bytes']/2**20:.1f} |")
    text+=['','## Full-dataset ANN','',
           'Recall@10 is against original f32 ground truth for both precisions. These are single Python query loops; each index was built once.','',
           '| Dataset | Precision | ef | Recall@10 | p50 ms | p95 ms | QPS | Peak RSS GiB | Snapshot GiB |',
           '|---|---|---:|---:|---:|---:|---:|---:|---:|']
    for d in ann:
        for r in d['results']:
            for v in r['runs']:
                text.append(f"| {d['dataset']['name']} | {r['case']['precision']} | {v['ef']} | {v['recall']:.4f} | {v['p50_ms']:.3f} | {v['p95_ms']:.3f} | {v['qps']:.0f} | {r['peak_rss_bytes']/2**30:.2f} | {r['snapshot_bytes']/2**30:.2f} |")
    text+=['','## Full-image costs','','One build per precision; scheduling disabled, warm-cache reopen. Vector bytes are engine counters; RSS is the full process high-water mark.','',
           '| Dataset | Precision | Build s | First save s | Checkpoint s | Warm open s | Vector MiB | Snapshot GiB | Peak RSS GiB |',
           '|---|---|---:|---:|---:|---:|---:|---:|---:|']
    for d in ann:
        for r in d['results']:
            text.append(f"| {d['dataset']['name']} | {r['case']['precision']} | {r['build_ms']/1000:.2f} | {r['save_ms']/1000:.2f} | {r['checkpoint_ms']/1000:.2f} | {r['warm_open_ms']/1000:.2f} | {r['collection_stats']['vector_bytes']/2**20:.1f} | {r['snapshot_bytes']/2**30:.2f} | {r['peak_rss_bytes']/2**30:.2f} |")
    text+=['','## Measured recall targets','','Fastest measured setting that reaches each target; no interpolation between ef values. “Not reached” describes this sweep, not a theoretical limit.','',
           '| Dataset | Precision | Recall target | ef | Measured recall | QPS |',
           '|---|---|---:|---:|---:|---:|']
    for d in ann:
        for r in d['results']:
            for target in (0.95,0.99):
                candidates=[v for v in r['runs'] if v['recall']>=target]
                if candidates:
                    v=max(candidates,key=lambda x:x['qps'])
                    text.append(f"| {d['dataset']['name']} | {r['case']['precision']} | {target:.2f} | {v['ef']} | {v['recall']:.4f} | {v['qps']:.0f} |")
                else:
                    text.append(f"| {d['dataset']['name']} | {r['case']['precision']} | {target:.2f} | Not reached | — | — |")
    if a.plots:
        import matplotlib
        matplotlib.use('Agg')
        import matplotlib.pyplot as plt
        plt.rcParams.update({'svg.hashsalt':'recern-vector-050','font.size':10})
        colors={(current,3):'#2368d1',(current,2):'#d17b13',('0.4.0',2):'#666666'}
        def save(fig,name):
            fig.savefig(folder/f'{name}.svg',bbox_inches='tight',metadata={'Date':None,'Creator':'bench/report_050.py'})
            fig.savefig(folder/f'{name}.png',dpi=180,bbox_inches='tight')
            plt.close(fig)
        fig,axes=plt.subplots(2,3,figsize=(12,6.5),layout='constrained')
        for row,precision in enumerate(('f32','int8')):
            for col,op in enumerate(('update','delete','no_op')):
                ax=axes[row,col]
                for candidate,color in colors.items():
                    points=sorted([r for r in rows if r['precision']==precision and (r['version'],r['format'])==candidate],key=lambda x:x['records'])
                    ax.plot([r['records'] for r in points],[r['phases'][op]['p50_ms'] for r in points],marker='o',color=color,label=f'{candidate[0]} / format {candidate[1]}')
                ax.set(xscale='log',yscale='log',title=f'{precision}: {op.replace("_"," ")}',xlabel='Records',ylabel='Save p50 (ms)')
                ax.grid(alpha=.2,which='both')
        axes[0,0].legend(fontsize=8)
        fig.suptitle('Recern Vector 0.5.0-dev.1: warm-cache saves, manual checkpoints, 128 dimensions')
        save(fig,'persistence')
        fig,axes=plt.subplots(1,2,figsize=(10,4),layout='constrained')
        for ax,d in zip(axes,ann):
            for r,color in zip(d['results'],['#2368d1','#8850bc']):
                ax.plot([v['recall'] for v in r['runs']],[v['qps'] for v in r['runs']],marker='o',color=color,label=r['case']['precision'])
            ax.set(xlabel='Recall@10 vs original f32 neighbors',ylabel='Single-query QPS',yscale='log',xlim=(.45,1.01),xticks=[.5,.6,.7,.8,.9,1.],title='SIFT1M' if d['dataset']['name']=='sift' else 'GloVe-100')
            ax.grid(alpha=.2,which='both');ax.legend()
        fig.suptitle('Recern Vector 0.5.0-dev.1: full-dataset ANN, median of three query loops')
        save(fig,'ann')
        text+=['','## Plots','','![Save cost as databases grow](persistence.svg)','','![Recall and single-query throughput](ann.svg)']
    (folder/'RESULTS.md').write_text('\n'.join(text)+'\n')
    manifest={'schema':1,'source_commit':persist['source']['base_commit'],'source':persist['source'],
              'reports':{n:hashlib.sha256((folder/n).read_bytes()).hexdigest() for n in ['persistence.json','sift.json','glove.json','summary.json']},
              'reporter_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'matplotlib_version':matplotlib.__version__ if a.plots else None,
              'validated':True,'persistence_cases':len(persist['results']),'ann_records':{d['dataset']['name']:d['results'][0]['records'] for d in ann}}
    (folder/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print('Validated complete reports; wrote RESULTS.md, summary.json and manifest.json')


if __name__=='__main__':main()
